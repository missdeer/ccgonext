use crate::acp::{kill_acp_descendants, kill_process, AcpClient};
use crate::config::AgentConfig;
use crate::events::{EventLog, EventPayload};
use crate::state::{ProcessState, TurnState};
use agent_client_protocol_schema::{PromptResponse, StopReason};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock};

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct SessionKey {
    pub agent: String,
    pub cwd: PathBuf,
}

pub struct AcpSession {
    pub id: String,
    pub key: SessionKey,
    process_state: RwLock<ProcessState>,
    turn_state: RwLock<TurnState>,
    client: RwLock<Option<Arc<AcpClient>>>,
    acp_session_id: RwLock<Option<agent_client_protocol_schema::SessionId>>,
    // Last spawned wrapper PID. Kept independently of `client` so that even if
    // `mark_dead` takes the client (and the spawned shutdown task races with
    // ccgo's own exit), `shutdown` still has a root pid to sweep descendants.
    child_root_pid: RwLock<Option<u32>>,
    config: AgentConfig,
    event_log: Arc<EventLog>,
    last_activity: RwLock<Instant>,
    turn_guard: Mutex<()>,
}

impl AcpSession {
    pub fn new(key: SessionKey, config: AgentConfig, event_log: Arc<EventLog>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            key,
            process_state: RwLock::new(ProcessState::Stopped),
            turn_state: RwLock::new(TurnState::Idle),
            client: RwLock::new(None),
            acp_session_id: RwLock::new(None),
            child_root_pid: RwLock::new(None),
            config,
            event_log,
            last_activity: RwLock::new(Instant::now()),
            turn_guard: Mutex::new(()),
        }
    }

    pub async fn process_state(&self) -> ProcessState {
        *self.process_state.read().await
    }

    pub async fn turn_state(&self) -> TurnState {
        *self.turn_state.read().await
    }

    async fn touch(&self) {
        *self.last_activity.write().await = Instant::now();
    }

    pub async fn last_activity(&self) -> Instant {
        *self.last_activity.read().await
    }

    async fn emit_state_change(&self, process: ProcessState, turn: TurnState) {
        self.event_log.append(
            &self.id,
            &self.key.agent,
            EventPayload::StateChange {
                process: process.to_string(),
                turn: turn.to_string(),
            },
        );
    }

    async fn set_states(&self, process: ProcessState, turn: TurnState) {
        *self.process_state.write().await = process;
        *self.turn_state.write().await = turn;
        self.emit_state_change(process, turn).await;
    }

    async fn mark_dead(&self) {
        let client = self.client.write().await.take();
        *self.acp_session_id.write().await = None;
        self.set_states(ProcessState::Dead, TurnState::Idle).await;
        if let Some(client) = client {
            // terminate() sends TerminateJobObject + a descendant sweep, but its
            // `wrapper_killed` check is based on the syscall return rather than
            // observed exit. Add an explicit kill_process(root_pid) as a non-
            // blocking safety net before we forget the PID, so even a silently-
            // failed start_kill doesn't strand cmd.exe.
            client.terminate().await;
            let root_pid = *self.child_root_pid.read().await;
            kill_acp_descendants(root_pid);
            kill_process(root_pid);
            // Clear the stored PID right now: the process is dead, and keeping
            // the PID around risks a later session.shutdown sweep hitting a
            // recycled PID belonging to an unrelated process.
            *self.child_root_pid.write().await = None;
            tokio::spawn(async move {
                client.shutdown().await;
            });
        }
    }

    async fn finalize_turn_response(&self, resp: &PromptResponse) {
        let stop_reason = stop_reason_to_string(resp.stop_reason);
        self.event_log.append(
            &self.id,
            &self.key.agent,
            EventPayload::TurnComplete { stop_reason },
        );
    }

    async fn ensure_running(&self) -> anyhow::Result<()> {
        let ps = *self.process_state.read().await;
        if ps == ProcessState::Running {
            return Ok(());
        }
        if !ps.can_start() {
            anyhow::bail!("Cannot start agent in state {}", ps);
        }

        self.set_states(ProcessState::Starting, TurnState::Idle)
            .await;

        let client = match AcpClient::spawn(
            &self.config.acp_command,
            &self.config.acp_args,
            &self.config.env_vars,
            &self.key.cwd,
            self.config.callback_policy.clone(),
            self.id.clone(),
            self.key.agent.clone(),
            self.event_log.clone(),
        )
        .await
        {
            Ok(c) => c,
            Err(e) => {
                self.set_states(ProcessState::Dead, TurnState::Idle).await;
                return Err(e);
            }
        };

        // Track the wrapper PID immediately so initialize/new_session failures
        // (which run client.shutdown()) still have a root pid available for the
        // tree sweep. Cleared on every failure path below.
        *self.child_root_pid.write().await = client.root_pid();

        if let Err(e) = client.initialize(&self.key.cwd).await {
            client.shutdown().await;
            *self.child_root_pid.write().await = None;
            self.set_states(ProcessState::Dead, TurnState::Idle).await;
            return Err(e);
        }

        let new_session = match client.new_session(&self.key.cwd).await {
            Ok(s) => s,
            Err(e) => {
                client.shutdown().await;
                *self.child_root_pid.write().await = None;
                self.set_states(ProcessState::Dead, TurnState::Idle).await;
                return Err(e);
            }
        };

        *self.acp_session_id.write().await = Some(new_session.session_id);
        *self.client.write().await = Some(Arc::new(client));
        self.set_states(ProcessState::Running, TurnState::Idle)
            .await;

        Ok(())
    }

    pub async fn ask(&self, message: String, timeout: Option<Duration>) -> anyhow::Result<String> {
        let _turn_guard = match self.turn_guard.try_lock() {
            Ok(guard) => guard,
            Err(_) => anyhow::bail!("Agent busy (turn state: {})", self.turn_state().await),
        };
        let current_turn = self.turn_state().await;
        if !current_turn.can_prompt() {
            anyhow::bail!("Agent busy (turn state: {})", current_turn);
        }

        self.touch().await;
        self.ensure_running().await?;

        self.set_states(ProcessState::Running, TurnState::Prompting)
            .await;

        let timeout = timeout.unwrap_or(Duration::from_secs(600));
        let start_seq = self.event_log.next_seq();

        let client = self
            .client
            .read()
            .await
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("No client"))?;
        let sid = self
            .acp_session_id
            .read()
            .await
            .clone()
            .ok_or_else(|| anyhow::anyhow!("No ACP session"))?;

        let prompt_future = client.prompt(&sid, &message);
        tokio::pin!(prompt_future);
        let attempt = tokio::select! {
            result = &mut prompt_future => PromptAttempt::Completed(result),
            _ = tokio::time::sleep(timeout) => PromptAttempt::TimedOut
        };

        match attempt {
            PromptAttempt::Completed(Ok(resp)) => {
                self.set_states(ProcessState::Running, TurnState::Idle)
                    .await;
                self.touch().await;
                self.finalize_turn_response(&resp).await;
                Ok(self.collect_message_text(start_seq).await)
            }
            PromptAttempt::Completed(Err(err)) => {
                if self.client_disconnected().await {
                    self.mark_dead().await;
                } else {
                    self.set_states(ProcessState::Running, TurnState::Idle)
                        .await;
                    self.touch().await;
                }
                Err(err)
            }
            PromptAttempt::TimedOut => {
                self.mark_dead().await;
                Err(anyhow::anyhow!("Request timed out"))
            }
        }
    }

    async fn client_disconnected(&self) -> bool {
        let client = self.client.read().await;
        client.as_ref().map(|c| !c.is_connected()).unwrap_or(true)
    }

    async fn collect_message_text(&self, start_seq: u64) -> String {
        let events = self.event_log.replay_from(start_seq);
        match events {
            crate::events::ReplayResult::Complete(evts)
            | crate::events::ReplayResult::Partial { events: evts, .. } => evts
                .iter()
                .filter(|e| e.session_id == self.id)
                .filter_map(|e| match &e.payload {
                    EventPayload::MessageChunk { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(""),
        }
    }

    pub async fn shutdown(&self) {
        let root_pid = *self.child_root_pid.read().await;
        tracing::info!(
            agent = %self.key.agent,
            session = %self.id,
            ?root_pid,
            "session shutdown begin"
        );
        let client = self.client.write().await.take();
        if let Some(c) = client {
            c.shutdown().await;
        } else if root_pid.is_some() {
            // mark_dead already took the client; the spawned shutdown task may
            // never finish if ccgo exits first. Kill the wrapper pid itself
            // (cmd.exe) and sweep descendants ourselves.
            tracing::info!(
                ?root_pid,
                "session shutdown: client already taken, killing root + sweeping descendants"
            );
            kill_acp_descendants(root_pid);
            kill_process(root_pid);
        }
        // Final unconditional sweep to catch anything that escaped.
        kill_acp_descendants(root_pid);
        *self.child_root_pid.write().await = None;
        *self.acp_session_id.write().await = None;
        self.set_states(ProcessState::Dead, TurnState::Idle).await;
        tracing::info!(
            agent = %self.key.agent,
            session = %self.id,
            "session shutdown done"
        );
    }

    pub async fn respond_to_permission(&self, perm_id: &str, granted: bool) {
        let client = self.client.read().await;
        if let Some(c) = client.as_ref() {
            c.callback_handler()
                .respond_permission(perm_id, granted)
                .await;
        }
    }
}

enum PromptAttempt {
    Completed(anyhow::Result<PromptResponse>),
    TimedOut,
}

fn stop_reason_to_string(stop_reason: StopReason) -> String {
    serde_json::to_value(stop_reason)
        .ok()
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .unwrap_or_else(|| "unknown".to_string())
}

pub struct SessionManager {
    sessions: RwLock<HashMap<SessionKey, Arc<AcpSession>>>,
    sessions_by_id: RwLock<HashMap<String, Arc<AcpSession>>>,
    configs: HashMap<String, AgentConfig>,
    event_log: Arc<EventLog>,
}

impl SessionManager {
    pub fn new(configs: HashMap<String, AgentConfig>, event_log: Arc<EventLog>) -> Arc<Self> {
        let mgr = Arc::new(Self {
            sessions: RwLock::new(HashMap::new()),
            sessions_by_id: RwLock::new(HashMap::new()),
            configs,
            event_log,
        });

        let weak = Arc::downgrade(&mgr);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(60)).await;
                let Some(mgr) = weak.upgrade() else { break };
                mgr.reap_idle().await;
            }
        });

        mgr
    }

    pub async fn get_or_create(&self, agent: &str, cwd: &Path) -> anyhow::Result<Arc<AcpSession>> {
        let key = SessionKey {
            agent: agent.to_string(),
            cwd: cwd.to_path_buf(),
        };

        {
            let sessions = self.sessions.read().await;
            if let Some(s) = sessions.get(&key) {
                return Ok(s.clone());
            }
        }

        let config = self
            .configs
            .get(agent)
            .ok_or_else(|| anyhow::anyhow!("Unknown agent: {}", agent))?
            .clone();

        let mut sessions = self.sessions.write().await;
        if let Some(session) = sessions.get(&key) {
            return Ok(session.clone());
        }

        let session = Arc::new(AcpSession::new(key.clone(), config, self.event_log.clone()));
        let mut by_id = self.sessions_by_id.write().await;
        sessions.insert(key, session.clone());
        by_id.insert(session.id.clone(), session.clone());

        Ok(session)
    }

    pub async fn get_by_id(&self, session_id: &str) -> Option<Arc<AcpSession>> {
        self.sessions_by_id.read().await.get(session_id).cloned()
    }

    pub async fn list_sessions(&self) -> Vec<crate::web::SessionInfo> {
        let sessions: Vec<(SessionKey, Arc<AcpSession>)> = self
            .sessions
            .read()
            .await
            .iter()
            .map(|(key, session)| (key.clone(), session.clone()))
            .collect();
        let mut result = Vec::new();
        for (key, session) in sessions {
            let ps = session.process_state().await;
            let ts = session.turn_state().await;
            result.push(crate::web::SessionInfo {
                id: session.id.clone(),
                agent: key.agent.clone(),
                cwd: key.cwd.to_string_lossy().to_string(),
                process_state: ps.to_string(),
                turn_state: ts.to_string(),
            });
        }
        result
    }

    pub async fn get_all_status(&self) -> Vec<(String, ProcessState, TurnState)> {
        let sessions: Vec<(SessionKey, Arc<AcpSession>)> = self
            .sessions
            .read()
            .await
            .iter()
            .map(|(key, session)| (key.clone(), session.clone()))
            .collect();
        let mut result = Vec::new();
        for (key, session) in sessions {
            let ps = session.process_state().await;
            let ts = session.turn_state().await;
            result.push((key.agent.clone(), ps, ts));
        }
        result
    }

    pub async fn shutdown_all(&self) {
        let sessions = {
            let mut sessions = self.sessions.write().await;
            let drained = sessions
                .drain()
                .map(|(_, session)| session)
                .collect::<Vec<_>>();
            self.sessions_by_id.write().await.clear();
            drained
        };

        for session in sessions {
            session.shutdown().await;
        }
    }

    async fn reap_idle(&self) {
        let sessions: Vec<(SessionKey, Arc<AcpSession>)> = self
            .sessions
            .read()
            .await
            .iter()
            .map(|(key, session)| (key.clone(), session.clone()))
            .collect();
        let mut to_reap = Vec::new();

        for (key, session) in sessions {
            let ps = session.process_state().await;
            if ps != ProcessState::Running {
                continue;
            }
            let ts = session.turn_state().await;
            if ts != TurnState::Idle {
                continue;
            }
            let elapsed = session.last_activity().await.elapsed();
            if elapsed > session.config.idle_timeout {
                to_reap.push(key.clone());
            }
        }

        for key in to_reap {
            if let Some(session) = self.remove_session(&key).await {
                tracing::info!(agent = %key.agent, "Reaping idle session {}", session.id);
                session.shutdown().await;
            }
        }
    }

    pub fn event_log(&self) -> &Arc<EventLog> {
        &self.event_log
    }

    pub fn has_agent(&self, name: &str) -> bool {
        self.configs.contains_key(name)
    }

    async fn remove_session(&self, key: &SessionKey) -> Option<Arc<AcpSession>> {
        let session = self.sessions.write().await.remove(key);
        if let Some(session) = &session {
            self.sessions_by_id.write().await.remove(&session.id);
        }
        session
    }
}
