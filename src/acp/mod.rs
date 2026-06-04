pub mod callbacks;
pub mod process;
pub mod protocol;

use agent_client_protocol_schema::{
    CancelNotification, ContentBlock, CreateTerminalRequest, InitializeRequest, InitializeResponse,
    KillTerminalRequest, NewSessionRequest, NewSessionResponse, PromptRequest, PromptResponse,
    ProtocolVersion, ReadTextFileRequest, ReleaseTerminalRequest, RequestPermissionRequest,
    SessionId, SessionNotification, SessionUpdate, TerminalOutputRequest, TextContent,
    ToolCallContent, WaitForTerminalExitRequest, WriteTextFileRequest, CLIENT_METHOD_NAMES,
};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::task::JoinHandle;

use crate::events::{EventLog, EventPayload};

use self::callbacks::{CallbackHandler, CallbackPolicy};
use self::process::{drain_stderr, AcpProcess};
use self::protocol::{
    IncomingKind, IncomingMessage, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
};

type ResponseResult = anyhow::Result<serde_json::Value>;
type SharedChild = Arc<Mutex<Option<Box<dyn process_wrap::tokio::TokioChildWrapper>>>>;

enum WriterMsg {
    Line(String),
    Shutdown,
}

pub struct AcpClient {
    writer_tx: mpsc::Sender<WriterMsg>,
    pending: Arc<Mutex<HashMap<i64, oneshot::Sender<ResponseResult>>>>,
    next_id: AtomicI64,
    writer_handle: Mutex<Option<JoinHandle<()>>>,
    reader_handle: Mutex<Option<JoinHandle<()>>>,
    wait_handle: Mutex<Option<JoinHandle<()>>>,
    stderr_handle: Mutex<Option<JoinHandle<()>>>,
    child: SharedChild,
    callback_handler: Arc<CallbackHandler>,
    connected: Arc<AtomicBool>,
}

impl AcpClient {
    #[allow(clippy::too_many_arguments)]
    pub async fn spawn(
        command: &str,
        args: &[String],
        env_vars: &HashMap<String, String>,
        cwd: &Path,
        policy: CallbackPolicy,
        session_id: String,
        agent_name: String,
        event_log: Arc<EventLog>,
    ) -> anyhow::Result<Self> {
        let proc = AcpProcess::spawn(command, args, env_vars, cwd)?;

        let (writer_tx, writer_rx) = mpsc::channel::<WriterMsg>(64);
        let pending: Arc<Mutex<HashMap<i64, oneshot::Sender<ResponseResult>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let connected = Arc::new(AtomicBool::new(true));

        let callback_handler = Arc::new(CallbackHandler::new(
            policy,
            session_id.clone(),
            agent_name.clone(),
            event_log.clone(),
            cwd.to_path_buf(),
        ));

        let child = Arc::new(Mutex::new(Some(proc.child)));

        let writer_handle = tokio::spawn(writer_task(
            proc.stdin,
            writer_rx,
            connected.clone(),
            child.clone(),
        ));

        let stderr_handle = tokio::spawn(drain_stderr(proc.stderr, agent_name.clone()));

        let reader_handle = tokio::spawn(reader_task(
            proc.stdout,
            pending.clone(),
            connected.clone(),
            callback_handler.clone(),
            writer_tx.clone(),
            child.clone(),
            event_log.clone(),
            session_id,
            agent_name,
        ));

        let wait_handle = tokio::spawn(wait_task(child.clone(), connected.clone()));

        Ok(Self {
            writer_tx,
            pending,
            next_id: AtomicI64::new(1),
            writer_handle: Mutex::new(Some(writer_handle)),
            reader_handle: Mutex::new(Some(reader_handle)),
            wait_handle: Mutex::new(Some(wait_handle)),
            stderr_handle: Mutex::new(Some(stderr_handle)),
            child,
            callback_handler,
            connected,
        })
    }

    async fn send_request<P: serde::Serialize>(
        &self,
        method: &'static str,
        params: P,
    ) -> anyhow::Result<serde_json::Value> {
        if !self.is_connected() {
            anyhow::bail!("ACP agent connection closed");
        }

        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let req = JsonRpcRequest::new(id.into(), method, params);
        let line = serde_json::to_string(&req)?;

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            pending.insert(id, tx);
        }

        if !self.is_connected() {
            let mut pending = self.pending.lock().await;
            if let Some(tx) = pending.remove(&id) {
                let _ = tx.send(Err(anyhow::anyhow!("ACP agent connection closed")));
            }
            anyhow::bail!("ACP agent connection closed");
        }

        if self.writer_tx.send(WriterMsg::Line(line)).await.is_err() {
            let mut pending = self.pending.lock().await;
            pending.remove(&id);
            anyhow::bail!("Writer channel closed");
        }

        rx.await
            .map_err(|_| anyhow::anyhow!("Response channel dropped"))?
    }

    async fn send_notification<P: serde::Serialize>(
        &self,
        method: &'static str,
        params: P,
    ) -> anyhow::Result<()> {
        let notif = JsonRpcNotification::new(method, params);
        let line = serde_json::to_string(&notif)?;
        self.writer_tx
            .send(WriterMsg::Line(line))
            .await
            .map_err(|_| anyhow::anyhow!("Writer channel closed"))?;
        Ok(())
    }

    pub async fn initialize(&self, _cwd: &Path) -> anyhow::Result<InitializeResponse> {
        let req = InitializeRequest::new(ProtocolVersion::LATEST).client_info(
            agent_client_protocol_schema::Implementation::new("ccgonext", "0.2.0"),
        );
        let result = self.send_request("initialize", req).await?;
        Ok(serde_json::from_value(result)?)
    }

    pub async fn new_session(&self, cwd: &Path) -> anyhow::Result<NewSessionResponse> {
        let req = NewSessionRequest::new(cwd.to_path_buf());
        let result = self.send_request("session/new", req).await?;
        Ok(serde_json::from_value(result)?)
    }

    pub async fn prompt(
        &self,
        session_id: &SessionId,
        text: &str,
    ) -> anyhow::Result<PromptResponse> {
        let req = PromptRequest::new(
            session_id.clone(),
            vec![ContentBlock::Text(TextContent::new(text))],
        );
        let result = self.send_request("session/prompt", req).await?;
        Ok(serde_json::from_value(result)?)
    }

    pub async fn cancel(&self, session_id: &SessionId) -> anyhow::Result<()> {
        let notif = CancelNotification::new(session_id.clone());
        self.send_notification("session/cancel", notif).await
    }

    pub fn callback_handler(&self) -> &Arc<CallbackHandler> {
        &self.callback_handler
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    pub async fn terminate(&self) {
        self.connected.store(false, Ordering::SeqCst);
        start_kill_child(&self.child).await;
    }

    pub async fn shutdown(&self) {
        self.callback_handler.shutdown().await;
        self.connected.store(false, Ordering::SeqCst);
        let writer = self.writer_handle.lock().await.take();
        let reader = self.reader_handle.lock().await.take();
        let stderr = self.stderr_handle.lock().await.take();
        let wait = self.wait_handle.lock().await.take();
        let _ = self.writer_tx.try_send(WriterMsg::Shutdown);
        if let Some(h) = &writer {
            h.abort();
        }
        if let Some(h) = &reader {
            h.abort();
        }
        if let Some(h) = &stderr {
            h.abort();
        }
        if let Some(h) = &wait {
            h.abort();
        }
        if let Some(h) = wait {
            let _ = h.await;
        }
        kill_child(&self.child).await;
        if let Some(h) = writer {
            let _ = h.await;
        }
        if let Some(h) = reader {
            let _ = h.await;
        }
        if let Some(h) = stderr {
            let _ = h.await;
        }
        fail_pending_requests(&self.pending, "ACP client shutdown".to_string()).await;
    }
}

impl Drop for AcpClient {
    fn drop(&mut self) {
        self.connected.store(false, Ordering::SeqCst);
        if let Ok(mut handle) = self.writer_handle.try_lock() {
            if let Some(h) = handle.take() {
                h.abort();
            }
        }
        if let Ok(mut handle) = self.reader_handle.try_lock() {
            if let Some(h) = handle.take() {
                h.abort();
            }
        }
        if let Ok(mut handle) = self.stderr_handle.try_lock() {
            if let Some(h) = handle.take() {
                h.abort();
            }
        }
        if let Ok(mut handle) = self.wait_handle.try_lock() {
            if let Some(h) = handle.take() {
                h.abort();
            }
        }
        if let Ok(mut child) = self.child.try_lock() {
            if let Some(c) = child.as_mut() {
                let _ = c.start_kill();
            }
        }
    }
}

async fn start_kill_child(child: &SharedChild) {
    let mut guard = child.lock().await;
    if let Some(c) = guard.as_mut() {
        let _ = c.start_kill();
    }
}

async fn fail_pending_requests(
    pending: &Arc<Mutex<HashMap<i64, oneshot::Sender<ResponseResult>>>>,
    message: String,
) {
    let mut pending = pending.lock().await;
    for (_, tx) in pending.drain() {
        let _ = tx.send(Err(anyhow::anyhow!(message.clone())));
    }
}

async fn writer_task(
    mut stdin: tokio::process::ChildStdin,
    mut rx: mpsc::Receiver<WriterMsg>,
    connected: Arc<AtomicBool>,
    child: SharedChild,
) {
    let mut needs_child_cleanup = false;
    while let Some(msg) = rx.recv().await {
        match msg {
            WriterMsg::Line(line) => {
                let data = format!("{}\n", line);
                if stdin.write_all(data.as_bytes()).await.is_err() {
                    needs_child_cleanup = true;
                    break;
                }
                if stdin.flush().await.is_err() {
                    needs_child_cleanup = true;
                    break;
                }
            }
            WriterMsg::Shutdown => {
                let _ = stdin.shutdown().await;
                break;
            }
        }
    }

    connected.store(false, Ordering::SeqCst);
    if needs_child_cleanup {
        kill_child(&child).await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn reader_task(
    mut stdout: tokio::io::BufReader<tokio::process::ChildStdout>,
    pending: Arc<Mutex<HashMap<i64, oneshot::Sender<ResponseResult>>>>,
    connected: Arc<AtomicBool>,
    callback_handler: Arc<CallbackHandler>,
    writer_tx: mpsc::Sender<WriterMsg>,
    child: SharedChild,
    event_log: Arc<EventLog>,
    session_id: String,
    agent_name: String,
) {
    let request_semaphore = Arc::new(tokio::sync::Semaphore::new(16));
    let mut line = String::new();
    loop {
        line.clear();
        match stdout.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                let msg: IncomingMessage = match serde_json::from_str(trimmed) {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::debug!(agent = %agent_name, "Invalid JSON from agent: {}", e);
                        continue;
                    }
                };

                match msg.classify() {
                    IncomingKind::Response { id, result, error } => {
                        let numeric_id = match &id {
                            agent_client_protocol_schema::RequestId::Number(n) => *n,
                            _ => continue,
                        };
                        let mut pending = pending.lock().await;
                        pending.retain(|_, tx| !tx.is_closed());
                        if let Some(tx) = pending.remove(&numeric_id) {
                            let response = if let Some(err) = error {
                                Err(anyhow::anyhow!("RPC error {}: {}", err.code, err.message))
                            } else {
                                Ok(result.unwrap_or(serde_json::Value::Null))
                            };
                            let _ = tx.send(response);
                        }
                    }
                    IncomingKind::Notification { method, params } => {
                        if method == "session/update" {
                            if let Ok(notif) = serde_json::from_value::<SessionNotification>(params)
                            {
                                handle_session_update(
                                    &event_log,
                                    &session_id,
                                    &agent_name,
                                    &notif.update,
                                )
                                .await;
                            }
                        }
                    }
                    IncomingKind::Request { id, method, params } => {
                        let handler = callback_handler.clone();
                        let tx = writer_tx.clone();
                        let sem = request_semaphore.clone();
                        tokio::spawn(async move {
                            let _permit = match sem.acquire().await {
                                Ok(p) => p,
                                Err(_) => return,
                            };
                            let response = handle_agent_request(&method, params, &handler).await;

                            let resp = JsonRpcResponse {
                                jsonrpc: "2.0",
                                id,
                                result: response.as_ref().ok().cloned(),
                                error: response.err().map(|e| protocol::JsonRpcError {
                                    code: -32000,
                                    message: e.to_string(),
                                    data: None,
                                }),
                            };

                            if let Ok(line) = serde_json::to_string(&resp) {
                                let _ = tx.send(WriterMsg::Line(line)).await;
                            }
                        });
                    }
                    IncomingKind::Invalid => {
                        tracing::debug!(agent = %agent_name, "Invalid JSON-RPC message");
                    }
                }
            }
            Err(e) => {
                tracing::debug!(agent = %agent_name, "stdout read error: {}", e);
                break;
            }
        }
    }

    connected.store(false, Ordering::SeqCst);
    kill_child(&child).await;
    fail_pending_requests(
        &pending,
        format!("ACP agent connection closed for {}", agent_name),
    )
    .await;
}

async fn handle_agent_request(
    method: &str,
    params: serde_json::Value,
    handler: &CallbackHandler,
) -> Result<serde_json::Value, String> {
    match method {
        m if m == CLIENT_METHOD_NAMES.session_request_permission => {
            let req: RequestPermissionRequest =
                serde_json::from_value(params).map_err(|e| e.to_string())?;
            let resp = handler.handle_request_permission(req).await;
            serde_json::to_value(resp).map_err(|e| e.to_string())
        }
        m if m == CLIENT_METHOD_NAMES.fs_read_text_file => {
            let req: ReadTextFileRequest =
                serde_json::from_value(params).map_err(|e| e.to_string())?;
            match handler.handle_read_file(req).await {
                Ok(resp) => serde_json::to_value(resp).map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            }
        }
        m if m == CLIENT_METHOD_NAMES.fs_write_text_file => {
            let req: WriteTextFileRequest =
                serde_json::from_value(params).map_err(|e| e.to_string())?;
            match handler.handle_write_file(req).await {
                Ok(resp) => serde_json::to_value(resp).map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            }
        }
        m if m == CLIENT_METHOD_NAMES.terminal_create => {
            let req: CreateTerminalRequest =
                serde_json::from_value(params).map_err(|e| e.to_string())?;
            match handler.handle_create_terminal(req).await {
                Ok(resp) => serde_json::to_value(resp).map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            }
        }
        m if m == CLIENT_METHOD_NAMES.terminal_output => {
            let req: TerminalOutputRequest =
                serde_json::from_value(params).map_err(|e| e.to_string())?;
            match handler.handle_terminal_output(req).await {
                Ok(resp) => serde_json::to_value(resp).map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            }
        }
        m if m == CLIENT_METHOD_NAMES.terminal_release => {
            let req: ReleaseTerminalRequest =
                serde_json::from_value(params).map_err(|e| e.to_string())?;
            match handler.handle_release_terminal(req).await {
                Ok(resp) => serde_json::to_value(resp).map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            }
        }
        m if m == CLIENT_METHOD_NAMES.terminal_wait_for_exit => {
            let req: WaitForTerminalExitRequest =
                serde_json::from_value(params).map_err(|e| e.to_string())?;
            match handler.handle_wait_for_terminal_exit(req).await {
                Ok(resp) => serde_json::to_value(resp).map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            }
        }
        m if m == CLIENT_METHOD_NAMES.terminal_kill => {
            let req: KillTerminalRequest =
                serde_json::from_value(params).map_err(|e| e.to_string())?;
            match handler.handle_kill_terminal(req).await {
                Ok(resp) => serde_json::to_value(resp).map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            }
        }
        _ => Err(format!("Unknown method: {}", method)),
    }
}

async fn handle_session_update(
    event_log: &EventLog,
    session_id: &str,
    agent_name: &str,
    update: &SessionUpdate,
) {
    let payload = match update {
        SessionUpdate::AgentMessageChunk(chunk) => {
            let text = match &chunk.content {
                ContentBlock::Text(t) => t.text.as_str(),
                _ => return,
            };
            if text.is_empty() {
                return;
            }
            EventPayload::MessageChunk {
                text: text.to_string(),
            }
        }
        SessionUpdate::AgentThoughtChunk(chunk) => {
            let text = match &chunk.content {
                ContentBlock::Text(t) => t.text.as_str(),
                _ => return,
            };
            if text.is_empty() {
                return;
            }
            EventPayload::ThoughtChunk {
                text: text.to_string(),
            }
        }
        SessionUpdate::ToolCall(tc) => EventPayload::ToolCall {
            id: tc.tool_call_id.to_string(),
            title: tc.title.clone(),
            status: "in_progress".to_string(),
        },
        SessionUpdate::ToolCallUpdate(tcu) => EventPayload::ToolCallUpdate {
            id: tcu.tool_call_id.to_string(),
            status: serialize_optional_enum(&tcu.fields.status),
            output: format_tool_call_output(&tcu.fields),
        },
        SessionUpdate::Plan(plan) => {
            let entries = plan
                .entries
                .iter()
                .map(|e| crate::events::PlanEntry {
                    content: e.content.clone(),
                    status: format!("{:?}", e.status),
                })
                .collect();
            EventPayload::Plan { entries }
        }
        _ => return,
    };

    event_log.append(session_id, agent_name, payload);
}

fn serialize_optional_enum<T: serde::Serialize>(value: &Option<T>) -> String {
    value
        .as_ref()
        .and_then(|inner| serde_json::to_value(inner).ok())
        .and_then(|serialized| serialized.as_str().map(ToOwned::to_owned))
        .unwrap_or_else(|| "unknown".to_string())
}

fn format_tool_call_output(
    fields: &agent_client_protocol_schema::ToolCallUpdateFields,
) -> Option<String> {
    if let Some(content) = &fields.content {
        let parts = content
            .iter()
            .filter_map(|item| match item {
                ToolCallContent::Content(content) => match &content.content {
                    ContentBlock::Text(text) => Some(text.text.clone()),
                    other => serde_json::to_string(other).ok(),
                },
                ToolCallContent::Diff(diff) => serde_json::to_string(diff).ok(),
                ToolCallContent::Terminal(terminal) => {
                    Some(format!("terminal: {}", terminal.terminal_id))
                }
                _ => None,
            })
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        if !parts.is_empty() {
            return Some(parts.join("\n"));
        }
    }

    fields.raw_output.as_ref().map(|raw_output| {
        raw_output
            .as_str()
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| raw_output.to_string())
    })
}

async fn wait_task(child: SharedChild, connected: Arc<AtomicBool>) {
    loop {
        {
            let mut guard = child.lock().await;
            let Some(c) = guard.as_mut() else { break };
            match c.try_wait() {
                Ok(Some(_)) | Err(_) => break,
                Ok(None) => {}
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    connected.store(false, Ordering::SeqCst);
    kill_child(&child).await;
}

async fn kill_child(child: &SharedChild) {
    let taken_child = child.lock().await.take();
    if let Some(mut c) = taken_child {
        let _ = c.start_kill();
        let _ = tokio::task::spawn_blocking(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                match c.try_wait() {
                    Ok(Some(_)) | Err(_) => return,
                    Ok(None) if std::time::Instant::now() >= deadline => {
                        tracing::warn!("Timed out waiting for ACP process tree to exit after kill");
                        return;
                    }
                    Ok(None) => std::thread::sleep(std::time::Duration::from_millis(50)),
                }
            }
        })
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol_schema::{TextContent, ToolCallContent, ToolCallUpdateFields};

    #[tokio::test]
    async fn test_handle_agent_request_accepts_schema_method_names() {
        let temp = tempfile::tempdir().unwrap();
        let file_path = temp.path().join("note.txt");
        std::fs::write(&file_path, "hello").unwrap();

        let handler = CallbackHandler::new(
            CallbackPolicy::ReadOnly,
            "session-1".into(),
            "codex".into(),
            Arc::new(EventLog::new(100)),
            temp.path().to_path_buf(),
        );

        let params =
            serde_json::to_value(ReadTextFileRequest::new("session-1", file_path)).unwrap();
        let result = handle_agent_request(CLIENT_METHOD_NAMES.fs_read_text_file, params, &handler)
            .await
            .unwrap();

        assert_eq!(result["content"], "hello");
    }

    #[test]
    fn test_format_tool_call_output_prefers_text_content() {
        let fields = ToolCallUpdateFields::new().content(vec![ToolCallContent::Content(
            agent_client_protocol_schema::Content::new(ContentBlock::Text(TextContent::new(
                "tool output",
            ))),
        )]);

        assert_eq!(
            format_tool_call_output(&fields),
            Some("tool output".to_string())
        );
    }

    #[test]
    fn test_serialize_optional_enum_uses_wire_name() {
        let status = Some(agent_client_protocol_schema::ToolCallStatus::Completed);

        assert_eq!(serialize_optional_enum(&status), "completed");
    }
}
