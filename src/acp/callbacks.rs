use agent_client_protocol_schema::{
    CreateTerminalRequest, CreateTerminalResponse, KillTerminalRequest, KillTerminalResponse,
    PermissionOptionId, ReadTextFileRequest, ReadTextFileResponse, ReleaseTerminalRequest,
    ReleaseTerminalResponse, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, TerminalExitStatus, TerminalId,
    TerminalOutputRequest, TerminalOutputResponse, WaitForTerminalExitRequest,
    WaitForTerminalExitResponse, WriteTextFileRequest, WriteTextFileResponse,
};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::{oneshot, Notify, RwLock};

use crate::events::{EventLog, EventPayload};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum CallbackPolicy {
    DenyAll,
    ReadOnly,
    Ask,
    #[default]
    AutoApprove,
}

pub struct PendingPermission {
    pub tx: oneshot::Sender<bool>,
}

struct TerminalBuffer {
    output: String,
    truncated: bool,
}

struct ManagedTerminal {
    buffer: RwLock<TerminalBuffer>,
    exit_status: RwLock<Option<TerminalExitStatus>>,
    exit_notify: Notify,
    kill_tx: std::sync::Mutex<Option<oneshot::Sender<()>>>,
    output_limit: Option<usize>,
}

impl ManagedTerminal {
    fn new(output_limit: Option<u64>) -> Self {
        Self {
            buffer: RwLock::new(TerminalBuffer {
                output: String::new(),
                truncated: false,
            }),
            exit_status: RwLock::new(None),
            exit_notify: Notify::new(),
            kill_tx: std::sync::Mutex::new(None),
            output_limit: output_limit.and_then(|limit| usize::try_from(limit).ok()),
        }
    }
}

pub struct CallbackHandler {
    policy: CallbackPolicy,
    session_id: String,
    agent_name: String,
    event_log: Arc<EventLog>,
    cwd: PathBuf,
    pub pending_permissions: Arc<RwLock<HashMap<String, PendingPermission>>>,
    terminals: Arc<RwLock<HashMap<String, Arc<ManagedTerminal>>>>,
}

impl CallbackHandler {
    pub fn new(
        policy: CallbackPolicy,
        session_id: String,
        agent_name: String,
        event_log: Arc<EventLog>,
        cwd: PathBuf,
    ) -> Self {
        Self {
            policy,
            session_id,
            agent_name,
            event_log,
            cwd,
            pending_permissions: Arc::new(RwLock::new(HashMap::new())),
            terminals: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    fn resolve_path(
        &self,
        path: &std::path::Path,
        allow_missing_leaf: bool,
    ) -> Result<PathBuf, agent_client_protocol_schema::Error> {
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.cwd.join(path)
        };

        let canonical = if allow_missing_leaf && !absolute.exists() {
            let parent = absolute.parent().ok_or_else(|| {
                agent_client_protocol_schema::Error::new(-32002, "Path has no parent")
            })?;
            let canonical_parent = parent.canonicalize().map_err(|e| {
                agent_client_protocol_schema::Error::new(
                    -32002,
                    format!("Path resolve failed: {}", e),
                )
            })?;
            let file_name = absolute.file_name().ok_or_else(|| {
                agent_client_protocol_schema::Error::new(-32002, "Path has no file name")
            })?;
            canonical_parent.join(file_name)
        } else {
            absolute.canonicalize().map_err(|e| {
                agent_client_protocol_schema::Error::new(
                    -32002,
                    format!("Path resolve failed: {}", e),
                )
            })?
        };

        let cwd_canonical = self.cwd.canonicalize().unwrap_or_else(|_| self.cwd.clone());
        if !canonical.starts_with(&cwd_canonical) {
            return Err(agent_client_protocol_schema::Error::new(
                -32001,
                "Path outside project directory",
            ));
        }

        Ok(absolute)
    }

    fn validate_path(
        &self,
        path: &std::path::Path,
        allow_missing_leaf: bool,
    ) -> Result<(), agent_client_protocol_schema::Error> {
        let _ = self.resolve_path(path, allow_missing_leaf)?;
        Ok(())
    }

    async fn request_user_permission(&self, method: &str, description: String) -> bool {
        let perm_id = uuid::Uuid::new_v4().to_string();

        self.event_log.append(
            &self.session_id,
            &self.agent_name,
            EventPayload::PermissionRequest {
                id: perm_id.clone(),
                method: method.to_string(),
                description,
            },
        );

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending_permissions.write().await;
            pending.insert(perm_id.clone(), PendingPermission { tx });
        }

        let granted = tokio::time::timeout(std::time::Duration::from_secs(60), rx)
            .await
            .ok()
            .and_then(|result| result.ok())
            .unwrap_or(false);

        {
            let mut pending = self.pending_permissions.write().await;
            pending.remove(&perm_id);
        }

        self.event_log.append(
            &self.session_id,
            &self.agent_name,
            EventPayload::PermissionResponse {
                id: perm_id,
                granted,
            },
        );

        granted
    }

    async fn with_write_permission(
        &self,
        method: &str,
        description: String,
    ) -> agent_client_protocol_schema::Result<()> {
        match self.policy {
            CallbackPolicy::DenyAll | CallbackPolicy::ReadOnly => Err(
                agent_client_protocol_schema::Error::new(-32001, "Operation denied by policy"),
            ),
            CallbackPolicy::AutoApprove => Ok(()),
            CallbackPolicy::Ask => {
                if self.request_user_permission(method, description).await {
                    Ok(())
                } else {
                    Err(agent_client_protocol_schema::Error::new(
                        -32001,
                        "Operation denied by user",
                    ))
                }
            }
        }
    }

    async fn get_terminal(
        &self,
        terminal_id: &TerminalId,
    ) -> agent_client_protocol_schema::Result<Arc<ManagedTerminal>> {
        self.terminals
            .read()
            .await
            .get(terminal_id.0.as_ref())
            .cloned()
            .ok_or_else(|| agent_client_protocol_schema::Error::new(-32004, "Terminal not found"))
    }

    pub async fn handle_request_permission(
        &self,
        req: RequestPermissionRequest,
    ) -> RequestPermissionResponse {
        let deny_option = req
            .options
            .iter()
            .find(|o| {
                matches!(
                    o.kind,
                    agent_client_protocol_schema::PermissionOptionKind::RejectOnce
                        | agent_client_protocol_schema::PermissionOptionKind::RejectAlways
                )
            })
            .map(|o| o.option_id.clone());

        let approve_option = req
            .options
            .iter()
            .find(|o| {
                matches!(
                    o.kind,
                    agent_client_protocol_schema::PermissionOptionKind::AllowOnce
                        | agent_client_protocol_schema::PermissionOptionKind::AllowAlways
                )
            })
            .map(|o| o.option_id.clone());

        let make_response = |option_id: PermissionOptionId| -> RequestPermissionResponse {
            RequestPermissionResponse::new(RequestPermissionOutcome::Selected(
                SelectedPermissionOutcome::new(option_id),
            ))
        };

        match self.policy {
            CallbackPolicy::DenyAll => {
                if let Some(deny_id) = deny_option {
                    make_response(deny_id)
                } else {
                    RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled)
                }
            }
            CallbackPolicy::ReadOnly => {
                if let Some(deny_id) = deny_option {
                    make_response(deny_id)
                } else {
                    RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled)
                }
            }
            CallbackPolicy::Ask => {
                let granted = self
                    .request_user_permission(
                        "session/request_permission",
                        format!("{:?}", req.tool_call),
                    )
                    .await;

                if granted {
                    if let Some(approve_id) = approve_option {
                        make_response(approve_id)
                    } else {
                        RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled)
                    }
                } else if let Some(deny_id) = deny_option {
                    make_response(deny_id)
                } else {
                    RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled)
                }
            }
            CallbackPolicy::AutoApprove => {
                if let Some(approve_id) = approve_option {
                    make_response(approve_id)
                } else if let Some(first) = req.options.first() {
                    make_response(first.option_id.clone())
                } else {
                    RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled)
                }
            }
        }
    }

    pub async fn handle_read_file(
        &self,
        req: ReadTextFileRequest,
    ) -> agent_client_protocol_schema::Result<ReadTextFileResponse> {
        match self.policy {
            CallbackPolicy::DenyAll => Err(agent_client_protocol_schema::Error::new(
                -32001,
                "File read denied by policy",
            )),
            _ => {
                let path = self.resolve_path(&req.path, false)?;
                let content = tokio::fs::read_to_string(&path).await.map_err(|e| {
                    agent_client_protocol_schema::Error::new(-32002, format!("Read failed: {}", e))
                })?;
                Ok(ReadTextFileResponse::new(content))
            }
        }
    }

    pub async fn handle_write_file(
        &self,
        req: WriteTextFileRequest,
    ) -> agent_client_protocol_schema::Result<WriteTextFileResponse> {
        let path = self.resolve_path(&req.path, true)?;
        self.with_write_permission("fs/write_text_file", format!("Write to {}", path.display()))
            .await?;
        tokio::fs::write(&path, &req.content).await.map_err(|e| {
            agent_client_protocol_schema::Error::new(-32002, format!("Write failed: {}", e))
        })?;
        Ok(WriteTextFileResponse::new())
    }

    pub async fn handle_create_terminal(
        &self,
        req: CreateTerminalRequest,
    ) -> agent_client_protocol_schema::Result<CreateTerminalResponse> {
        self.with_write_permission(
            "terminal/create",
            format!("Execute {} {:?}", req.command, req.args),
        )
        .await?;

        let cwd = match req.cwd.as_ref() {
            Some(cwd) => self.resolve_path(cwd, false)?,
            None => self.cwd.clone(),
        };
        self.validate_path(&cwd, false)?;

        let mut cmd = Command::new(&req.command);
        cmd.args(&req.args)
            .current_dir(cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        for env_var in &req.env {
            cmd.env(&env_var.name, &env_var.value);
        }

        let mut child = cmd.spawn().map_err(|e| {
            agent_client_protocol_schema::Error::new(
                -32002,
                format!("Terminal spawn failed: {}", e),
            )
        })?;

        let terminal = Arc::new(ManagedTerminal::new(req.output_byte_limit));
        let terminal_id = uuid::Uuid::new_v4().to_string();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let (kill_tx, kill_rx) = oneshot::channel();
        *terminal.kill_tx.lock().unwrap() = Some(kill_tx);

        self.terminals
            .write()
            .await
            .insert(terminal_id.clone(), terminal.clone());

        let mut drain_handles = Vec::new();
        if let Some(stdout) = stdout {
            drain_handles.push(tokio::spawn(drain_terminal_stream(
                stdout,
                terminal.clone(),
            )));
        }
        if let Some(stderr) = stderr {
            drain_handles.push(tokio::spawn(drain_terminal_stream(
                stderr,
                terminal.clone(),
            )));
        }
        tokio::spawn(supervise_terminal(child, terminal, kill_rx, drain_handles));

        Ok(CreateTerminalResponse::new(terminal_id))
    }

    pub async fn handle_terminal_output(
        &self,
        req: TerminalOutputRequest,
    ) -> agent_client_protocol_schema::Result<TerminalOutputResponse> {
        let terminal = self.get_terminal(&req.terminal_id).await?;
        let buffer = terminal.buffer.read().await;
        let exit_status = terminal.exit_status.read().await.clone();
        Ok(
            TerminalOutputResponse::new(buffer.output.clone(), buffer.truncated)
                .exit_status(exit_status),
        )
    }

    pub async fn handle_release_terminal(
        &self,
        req: ReleaseTerminalRequest,
    ) -> agent_client_protocol_schema::Result<ReleaseTerminalResponse> {
        let terminal = self
            .terminals
            .write()
            .await
            .remove(req.terminal_id.0.as_ref())
            .ok_or_else(|| {
                agent_client_protocol_schema::Error::new(-32004, "Terminal not found")
            })?;
        if let Some(kill_tx) = terminal.kill_tx.lock().unwrap().take() {
            let _ = kill_tx.send(());
        }
        Ok(ReleaseTerminalResponse::new())
    }

    pub async fn handle_kill_terminal(
        &self,
        req: KillTerminalRequest,
    ) -> agent_client_protocol_schema::Result<KillTerminalResponse> {
        let terminal = self.get_terminal(&req.terminal_id).await?;
        if let Some(kill_tx) = terminal.kill_tx.lock().unwrap().take() {
            let _ = kill_tx.send(());
        }
        Ok(KillTerminalResponse::new())
    }

    pub async fn handle_wait_for_terminal_exit(
        &self,
        req: WaitForTerminalExitRequest,
    ) -> agent_client_protocol_schema::Result<WaitForTerminalExitResponse> {
        let terminal = self.get_terminal(&req.terminal_id).await?;
        loop {
            let notified = terminal.exit_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            if let Some(exit_status) = terminal.exit_status.read().await.clone() {
                return Ok(WaitForTerminalExitResponse::new(exit_status));
            }
            notified.await;
        }
    }

    pub async fn respond_permission(&self, perm_id: &str, granted: bool) {
        let mut pending = self.pending_permissions.write().await;
        if let Some(p) = pending.remove(perm_id) {
            let _ = p.tx.send(granted);
        }
    }

    pub async fn cancel_all_pending(&self) {
        let mut pending = self.pending_permissions.write().await;
        for (_, p) in pending.drain() {
            let _ = p.tx.send(false);
        }
    }

    pub async fn shutdown(&self) {
        self.cancel_all_pending().await;

        let terminals = {
            let mut terminals = self.terminals.write().await;
            terminals
                .drain()
                .map(|(_, terminal)| terminal)
                .collect::<Vec<_>>()
        };

        for terminal in terminals {
            if let Some(kill_tx) = terminal.kill_tx.lock().unwrap().take() {
                let _ = kill_tx.send(());
            }
        }
    }
}

async fn drain_terminal_stream<R>(mut reader: R, terminal: Arc<ManagedTerminal>)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let mut buf = [0_u8; 4096];
    let mut leftover = Vec::new();
    loop {
        match reader.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                let data = if leftover.is_empty() {
                    &buf[..n]
                } else {
                    leftover.extend_from_slice(&buf[..n]);
                    leftover.as_slice()
                };

                let valid_len = match std::str::from_utf8(data) {
                    Ok(_) => data.len(),
                    Err(e) => e.valid_up_to(),
                };

                if valid_len > 0 {
                    let text = unsafe { std::str::from_utf8_unchecked(&data[..valid_len]) };
                    append_terminal_output(&terminal, text).await;
                }

                let remaining = &data[valid_len..];
                if remaining.is_empty() {
                    leftover.clear();
                } else if remaining.len() >= 4 {
                    let chunk = String::from_utf8_lossy(remaining);
                    append_terminal_output(&terminal, chunk.as_ref()).await;
                    leftover.clear();
                } else {
                    let saved = remaining.to_vec();
                    leftover.clear();
                    leftover = saved;
                }
            }
            Err(_) => break,
        }
    }

    if !leftover.is_empty() {
        let chunk = String::from_utf8_lossy(&leftover);
        append_terminal_output(&terminal, chunk.as_ref()).await;
    }
}

async fn append_terminal_output(terminal: &ManagedTerminal, chunk: &str) {
    let mut buffer = terminal.buffer.write().await;
    buffer.output.push_str(chunk);

    if let Some(limit) = terminal.output_limit {
        if buffer.output.len() > limit {
            let mut trim_at = buffer.output.len() - limit;
            while trim_at < buffer.output.len() && !buffer.output.is_char_boundary(trim_at) {
                trim_at += 1;
            }
            buffer.output.drain(..trim_at);
            buffer.truncated = true;
        }
    }
}

async fn supervise_terminal(
    mut child: tokio::process::Child,
    terminal: Arc<ManagedTerminal>,
    mut kill_rx: oneshot::Receiver<()>,
    drain_handles: Vec<tokio::task::JoinHandle<()>>,
) {
    let status = tokio::select! {
        result = child.wait() => result,
        _ = &mut kill_rx => {
            let _ = child.kill().await;
            child.wait().await
        }
    };

    for handle in drain_handles {
        let _ = handle.await;
    }

    let exit_status = match status {
        Ok(status) => TerminalExitStatus::new()
            .exit_code(status.code().and_then(|code| u32::try_from(code).ok()))
            .signal(None::<String>),
        Err(err) => TerminalExitStatus::new().signal(Some(format!("wait_error: {}", err))),
    };

    *terminal.exit_status.write().await = Some(exit_status);
    terminal.exit_notify.notify_waiters();
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol_schema::{
        CreateTerminalRequest, PermissionOption, PermissionOptionKind, ReleaseTerminalRequest,
        SessionId, TerminalOutputRequest, WaitForTerminalExitRequest, WriteTextFileRequest,
    };
    use std::sync::Arc;

    fn make_handler(policy: CallbackPolicy) -> CallbackHandler {
        let event_log = Arc::new(crate::events::EventLog::new(100));
        CallbackHandler::new(
            policy,
            "test-session".into(),
            "test-agent".into(),
            event_log,
            std::env::temp_dir(),
        )
    }

    #[test]
    fn test_callback_policy_default() {
        assert_eq!(CallbackPolicy::default(), CallbackPolicy::AutoApprove);
    }

    #[test]
    fn test_callback_policy_serde() {
        let json = serde_json::to_string(&CallbackPolicy::AutoApprove).unwrap();
        assert_eq!(json, "\"auto_approve\"");
        let parsed: CallbackPolicy = serde_json::from_str("\"deny_all\"").unwrap();
        assert_eq!(parsed, CallbackPolicy::DenyAll);
    }

    #[tokio::test]
    async fn test_deny_all_rejects_read() {
        let handler = make_handler(CallbackPolicy::DenyAll);
        let req = ReadTextFileRequest::new("s1", std::path::PathBuf::from("/nonexistent"));
        let result = handler.handle_read_file(req).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_read_only_allows_read() {
        let handler = make_handler(CallbackPolicy::ReadOnly);
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "hello").unwrap();
        let req = ReadTextFileRequest::new("s1", tmp.path().to_path_buf());
        let result = handler.handle_read_file(req).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().content, "hello");
    }

    #[tokio::test]
    async fn test_read_only_rejects_write() {
        let handler = make_handler(CallbackPolicy::ReadOnly);
        let req = WriteTextFileRequest::new("s1", std::path::PathBuf::from("/tmp/test"), "data");
        let result = handler.handle_write_file(req).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_auto_approve_allows_write() {
        let handler = make_handler(CallbackPolicy::AutoApprove);
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let req = WriteTextFileRequest::new("s1", tmp.path().to_path_buf(), "written");
        let result = handler.handle_write_file(req).await;
        assert!(result.is_ok());
        let content = std::fs::read_to_string(tmp.path()).unwrap();
        assert_eq!(content, "written");
    }

    #[tokio::test]
    async fn test_auto_approve_allows_creating_new_file() {
        let temp = tempfile::tempdir().unwrap();
        let event_log = Arc::new(crate::events::EventLog::new(100));
        let handler = CallbackHandler::new(
            CallbackPolicy::AutoApprove,
            "test-session".into(),
            "test-agent".into(),
            event_log,
            temp.path().to_path_buf(),
        );
        let target = temp.path().join("new-file.txt");

        let req = WriteTextFileRequest::new("s1", target.clone(), "created");
        handler.handle_write_file(req).await.unwrap();

        assert_eq!(std::fs::read_to_string(target).unwrap(), "created");
    }

    #[tokio::test]
    async fn test_read_only_denies_permission_requests() {
        let event_log = Arc::new(crate::events::EventLog::new(100));
        let handler = CallbackHandler::new(
            CallbackPolicy::ReadOnly,
            "test-session".into(),
            "test-agent".into(),
            event_log,
            std::env::temp_dir(),
        );

        let request = RequestPermissionRequest::new(
            SessionId::new("s1"),
            agent_client_protocol_schema::ToolCallUpdate::new(
                "tool-1",
                agent_client_protocol_schema::ToolCallUpdateFields::new(),
            ),
            vec![
                PermissionOption::new("allow-1", "Allow once", PermissionOptionKind::AllowOnce),
                PermissionOption::new("deny-1", "Reject once", PermissionOptionKind::RejectOnce),
            ],
        );

        let response = handler.handle_request_permission(request).await;
        match response.outcome {
            RequestPermissionOutcome::Selected(selected) => {
                assert_eq!(selected.option_id, "deny-1".into());
            }
            other => panic!("unexpected outcome: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_auto_approve_terminal_callbacks_work() {
        let handler = make_handler(CallbackPolicy::AutoApprove);
        let (command, args) = terminal_echo_command("hello-from-terminal");
        let create = CreateTerminalRequest::new("s1", command).args(args);
        let created = handler.handle_create_terminal(create).await.unwrap();

        let waited = handler
            .handle_wait_for_terminal_exit(WaitForTerminalExitRequest::new(
                "s1",
                created.terminal_id.clone(),
            ))
            .await
            .unwrap();
        assert_eq!(waited.exit_status.exit_code, Some(0));

        let output = handler
            .handle_terminal_output(TerminalOutputRequest::new(
                "s1",
                created.terminal_id.clone(),
            ))
            .await
            .unwrap();
        assert!(output.output.contains("hello-from-terminal"));

        handler
            .handle_release_terminal(ReleaseTerminalRequest::new("s1", created.terminal_id))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_terminal_output_is_truncated_to_limit() {
        let handler = make_handler(CallbackPolicy::AutoApprove);
        let (command, args) = terminal_echo_command("123456789");
        let create = CreateTerminalRequest::new("s1", command)
            .args(args)
            .output_byte_limit(5_u64);
        let created = handler.handle_create_terminal(create).await.unwrap();

        handler
            .handle_wait_for_terminal_exit(WaitForTerminalExitRequest::new(
                "s1",
                created.terminal_id.clone(),
            ))
            .await
            .unwrap();

        let output = handler
            .handle_terminal_output(TerminalOutputRequest::new(
                "s1",
                created.terminal_id.clone(),
            ))
            .await
            .unwrap();

        assert!(output.truncated);
        assert!(output.output.len() <= 5);
        assert!(output.output.contains("6789") || output.output.contains("789"));
    }

    #[tokio::test]
    async fn test_shutdown_releases_terminals() {
        let handler = make_handler(CallbackPolicy::AutoApprove);
        let (command, args) = terminal_echo_command("shutdown-check");
        let create = CreateTerminalRequest::new("s1", command).args(args);
        let created = handler.handle_create_terminal(create).await.unwrap();

        handler.shutdown().await;

        let result = handler
            .handle_terminal_output(TerminalOutputRequest::new("s1", created.terminal_id))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_respond_permission() {
        let handler = make_handler(CallbackPolicy::Ask);
        let (tx, rx) = tokio::sync::oneshot::channel();
        {
            let mut pending = handler.pending_permissions.write().await;
            pending.insert("perm-1".into(), PendingPermission { tx });
        }
        handler.respond_permission("perm-1", true).await;
        assert!(rx.await.unwrap());
    }

    #[tokio::test]
    async fn test_cancel_all_pending() {
        let handler = make_handler(CallbackPolicy::Ask);
        let (tx1, rx1) = tokio::sync::oneshot::channel();
        let (tx2, rx2) = tokio::sync::oneshot::channel();
        {
            let mut pending = handler.pending_permissions.write().await;
            pending.insert("p1".into(), PendingPermission { tx: tx1 });
            pending.insert("p2".into(), PendingPermission { tx: tx2 });
        }
        handler.cancel_all_pending().await;
        assert!(!rx1.await.unwrap());
        assert!(!rx2.await.unwrap());
        assert!(handler.pending_permissions.read().await.is_empty());
    }

    fn terminal_echo_command(message: &str) -> (String, Vec<String>) {
        if cfg!(windows) {
            (
                "cmd".to_string(),
                vec!["/C".to_string(), format!("echo {}", message)],
            )
        } else {
            (
                "sh".to_string(),
                vec!["-c".to_string(), format!("printf '{}\\n'", message)],
            )
        }
    }
}
