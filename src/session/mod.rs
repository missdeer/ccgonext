//! Session management layer

use crate::agent::{Agent, ClaudeCodeAgent};
use crate::config::TimeoutConfig;
use crate::log_provider::LogProvider;
use crate::pty::PtyHandle;
use crate::state::{AgentState, SideEffect, StateMachine, StateTransition, TransitionResult};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, oneshot, Mutex, RwLock};
use uuid::Uuid;

#[derive(Debug)]
pub struct Request {
    pub id: String,
    pub message: String,
    pub timeout: Duration,
    pub created_at: Instant,
    pub response_tx: oneshot::Sender<Result<String, SessionError>>,
}

impl Request {
    pub fn new(
        message: String,
        timeout: Duration,
        response_tx: oneshot::Sender<Result<String, SessionError>>,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            message,
            timeout,
            created_at: Instant::now(),
            response_tx,
        }
    }
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum SessionError {
    #[error("Agent not running")]
    NotRunning,
    #[error("Agent is busy")]
    Busy,
    #[error("Queue timeout")]
    QueueTimeout,
    #[error("Request timeout")]
    RequestTimeout,
    #[error("Agent stopped: {0}")]
    Stopped(String),
    #[error("Agent crashed: {0}")]
    Crashed(String),
    #[error("Invalid state transition: {0}")]
    InvalidTransition(String),
    #[error("PTY error: {0}")]
    PtyError(String),
}

pub struct AgentSession {
    pub name: String,
    pub state: RwLock<AgentState>,
    pub adapter: Arc<dyn Agent>,
    pub log_provider: Arc<dyn LogProvider>,
    pub pty: RwLock<Option<Arc<PtyHandle>>>,
    pub request_queue: Mutex<VecDeque<Request>>,
    pub current_request: Mutex<Option<Request>>,
    pub working_dir: PathBuf,
    pub timeouts: TimeoutConfig,
    pub restart_count: Mutex<u32>,
    pub last_restart: Mutex<Option<Instant>>,

    // Locks for concurrency control
    lifecycle_lock: Mutex<()>,
    request_queue_lock: Mutex<()>,
}

/// Prepared request info for sending after lock release
struct PreparedRequest {
    message_id: String,
    message_with_sentinel: String,
    baseline_offset: u64,
    request_timeout: Duration,
    pty_start_offset: u64, // For ClaudeCode PTY parsing
}

impl AgentSession {
    pub fn new(
        name: String,
        adapter: Arc<dyn Agent>,
        log_provider: Arc<dyn LogProvider>,
        working_dir: PathBuf,
        timeouts: TimeoutConfig,
    ) -> Self {
        Self {
            name,
            state: RwLock::new(AgentState::Stopped),
            adapter,
            log_provider,
            pty: RwLock::new(None),
            request_queue: Mutex::new(VecDeque::new()),
            current_request: Mutex::new(None),
            working_dir,
            timeouts,
            restart_count: Mutex::new(0),
            last_restart: Mutex::new(None),
            lifecycle_lock: Mutex::new(()),
            request_queue_lock: Mutex::new(()),
        }
    }

    pub async fn get_state(&self) -> AgentState {
        *self.state.read().await
    }

    async fn apply_transition(
        &self,
        event: StateTransition,
    ) -> Result<TransitionResult, SessionError> {
        // Use write lock for atomic read-modify-write
        let mut state_guard = self.state.write().await;
        let current = *state_guard;
        let result = StateMachine::transition(current, event.clone())
            .map_err(|e| SessionError::InvalidTransition(e.to_string()))?;

        *state_guard = result.new_state;
        drop(state_guard);

        // Process side effects
        for effect in &result.side_effects {
            self.handle_side_effect(effect).await?;
        }

        Ok(result)
    }

    async fn handle_side_effect(&self, effect: &SideEffect) -> Result<(), SessionError> {
        match effect {
            SideEffect::CreatePty => {
                // PTY creation is handled in start()
            }
            SideEffect::StartProcess => {
                // Process start is handled in start()
            }
            SideEffect::MarkReady => {
                tracing::info!("Agent {} is ready", self.name);
            }
            SideEffect::LogWarning(msg) => {
                tracing::warn!("{}: {}", self.name, msg);
            }
            SideEffect::SendMessage { message_id } => {
                tracing::debug!("Sending message {} to {}", message_id, self.name);
            }
            SideEffect::ReturnResult => {
                tracing::debug!("Returning result for {}", self.name);
            }
            SideEffect::ReturnTimeoutError => {
                tracing::warn!("Request timeout for {}", self.name);
            }
            SideEffect::StartBackgroundRecovery => {
                tracing::info!("Starting background recovery for {}", self.name);
            }
            SideEffect::KillProcess => {
                tracing::warn!("Killing process for {}", self.name);
            }
            SideEffect::ClearQueue => {
                let mut queue = self.request_queue.lock().await;
                while let Some(req) = queue.pop_front() {
                    let _ = req
                        .response_tx
                        .send(Err(SessionError::Stopped("Queue cleared".to_string())));
                }
            }
            SideEffect::NotifyWaiters(msg) => {
                tracing::info!("Notifying waiters for {}: {}", self.name, msg);
            }
            SideEffect::TriggerAutoRestart => {
                tracing::info!("Triggering auto-restart for {}", self.name);
            }
        }
        Ok(())
    }

    pub async fn start(
        self: &Arc<Self>,
        pty_manager: &crate::pty::PtyManager,
    ) -> Result<(), SessionError> {
        let _lifecycle = self.lifecycle_lock.lock().await;

        let current = self.get_state().await;
        if !current.can_start() {
            return Err(SessionError::InvalidTransition(format!(
                "Cannot start from state {}",
                current
            )));
        }

        // Apply start transition
        self.apply_transition(StateTransition::StartAgent).await?;

        // Get startup command
        let command = self.adapter.get_startup_command(&self.working_dir);

        // Create PTY with command - rollback state on failure
        let pty = match pty_manager
            .create(&self.name, &command, &self.working_dir)
            .await
        {
            Ok(pty) => pty,
            Err(e) => {
                // Rollback state to Dead on PTY creation failure
                *self.state.write().await = AgentState::Dead;
                tracing::error!("PTY creation failed for {}: {}", self.name, e);
                return Err(SessionError::PtyError(e.to_string()));
            }
        };

        // Subscribe to output BEFORE storing PTY to avoid missing initial output
        let pty_rx = pty.subscribe_output();

        *self.pty.write().await = Some(pty);

        // Start ready detection task with pre-subscribed receiver
        Arc::clone(self).start_ready_detection_with_rx(pty_rx).await;

        Ok(())
    }

    /// Start agent with retry logic and exponential backoff.
    /// Retries up to `max_start_retries` times with delays of:
    /// delay_ms, delay_ms*2, delay_ms*4, ... (exponential backoff)
    pub async fn start_with_retry(
        self: &Arc<Self>,
        pty_manager: &crate::pty::PtyManager,
    ) -> Result<(), SessionError> {
        let max_retries = self.timeouts.max_start_retries;
        let base_delay_ms = self.timeouts.start_retry_delay_ms;
        let mut last_error = None;

        for attempt in 0..=max_retries {
            if attempt > 0 {
                // Exponential backoff: base_delay * 2^(attempt-1)
                let shift = (attempt - 1).min(63);
                let delay_ms = base_delay_ms.saturating_mul(1u64 << shift);
                tracing::info!(
                    "Retrying agent {} start (attempt {}/{}), waiting {}ms",
                    self.name,
                    attempt + 1,
                    max_retries + 1,
                    delay_ms
                );
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }

            match self.start(pty_manager).await {
                Ok(()) => {
                    if attempt > 0 {
                        tracing::info!(
                            "Agent {} started successfully after {} retries",
                            self.name,
                            attempt
                        );
                    }
                    return Ok(());
                }
                Err(e) => {
                    tracing::warn!(
                        "Agent {} start failed (attempt {}/{}): {}",
                        self.name,
                        attempt + 1,
                        max_retries + 1,
                        e
                    );
                    last_error = Some(e);
                }
            }
        }

        // All retries exhausted
        Err(last_error.unwrap_or_else(|| {
            SessionError::PtyError("Start failed after all retries".to_string())
        }))
    }

    async fn start_ready_detection_with_rx(self: &Arc<Self>, rx: broadcast::Receiver<Vec<u8>>) {
        let ready_pattern = self.adapter.get_ready_pattern().to_string();
        let timeout = Duration::from_secs(self.timeouts.ready_check);
        let name = self.name.clone();
        let session = Arc::clone(self);

        let state = self.state.read().await;
        if *state != AgentState::Starting {
            return;
        }
        drop(state);

        // Compile regex pattern
        let pattern = match regex::Regex::new(&ready_pattern) {
            Ok(re) => re,
            Err(e) => {
                tracing::warn!(
                    "Invalid ready pattern '{}' for {}: {}, using substring match",
                    ready_pattern,
                    name,
                    e
                );
                // Fall back to substring matching if regex is invalid
                regex::Regex::new(&regex::escape(&ready_pattern)).unwrap()
            }
        };

        let mut rx = rx;

        tokio::spawn(async move {
            let deadline = Instant::now() + timeout;
            let poll_interval = Duration::from_millis(100);

            // Accumulate output for pattern matching across chunks
            let mut accumulated = String::new();

            while Instant::now() < deadline {
                // Check for ready pattern in PTY output
                match tokio::time::timeout(poll_interval, rx.recv()).await {
                    Ok(Ok(data)) => {
                        let text = String::from_utf8_lossy(&data);
                        accumulated.push_str(&text);
                        tracing::debug!("PTY output for {}: {:?}", name, text);

                        // Check both current chunk and accumulated output
                        if pattern.is_match(&text) || pattern.is_match(&accumulated) {
                            tracing::info!("Ready pattern detected for {}", name);
                            if let Err(e) = session
                                .apply_transition(StateTransition::ReadyDetected)
                                .await
                            {
                                tracing::warn!("Failed to apply ReadyDetected: {}", e);
                            }
                            return;
                        }
                    }
                    Ok(Err(_)) => break, // Channel closed
                    Err(_) => continue,  // Timeout, keep polling
                }
            }

            // Ready detection timed out
            tracing::warn!("Ready detection timed out for {}", name);
            if let Err(e) = session
                .apply_transition(StateTransition::ReadyTimeout)
                .await
            {
                tracing::warn!("Failed to apply ReadyTimeout: {}", e);
            }
        });
    }

    #[allow(dead_code)]
    async fn start_ready_detection(self: &Arc<Self>) {
        let ready_pattern = self.adapter.get_ready_pattern().to_string();
        let timeout = Duration::from_secs(self.timeouts.ready_check);
        let name = self.name.clone();
        let session = Arc::clone(self);

        let state = self.state.read().await;
        if *state != AgentState::Starting {
            return;
        }
        drop(state);

        // Compile regex pattern
        let pattern = match regex::Regex::new(&ready_pattern) {
            Ok(re) => re,
            Err(e) => {
                tracing::warn!(
                    "Invalid ready pattern '{}' for {}: {}, using substring match",
                    ready_pattern,
                    name,
                    e
                );
                // Fall back to substring matching if regex is invalid
                regex::Regex::new(&regex::escape(&ready_pattern)).unwrap()
            }
        };

        // Get PTY output subscriber for ready detection
        let pty_rx = {
            let pty_guard = session.pty.read().await;
            pty_guard.as_ref().map(|p| p.subscribe_output())
        };

        tokio::spawn(async move {
            let deadline = Instant::now() + timeout;
            let poll_interval = Duration::from_millis(100);

            if let Some(mut rx) = pty_rx {
                // Also accumulate output for pattern matching across chunks
                let mut accumulated = String::new();

                while Instant::now() < deadline {
                    // Check for ready pattern in PTY output
                    match tokio::time::timeout(poll_interval, rx.recv()).await {
                        Ok(Ok(data)) => {
                            let text = String::from_utf8_lossy(&data);
                            accumulated.push_str(&text);
                            tracing::debug!("PTY output for {}: {:?}", name, text);

                            // Check both current chunk and accumulated output
                            if pattern.is_match(&text) || pattern.is_match(&accumulated) {
                                tracing::info!("Ready pattern detected for {}", name);
                                if let Err(e) = session
                                    .apply_transition(StateTransition::ReadyDetected)
                                    .await
                                {
                                    tracing::warn!("Failed to apply ReadyDetected: {}", e);
                                }
                                return;
                            }
                        }
                        Ok(Err(_)) => break, // Channel closed
                        Err(_) => continue,  // Timeout, keep polling
                    }
                }
            }

            // Ready detection timed out
            tracing::warn!("Ready detection timed out for {}", name);
            if let Err(e) = session
                .apply_transition(StateTransition::ReadyTimeout)
                .await
            {
                tracing::warn!("Failed to apply ReadyTimeout: {}", e);
            }
        });
    }

    pub async fn stop(
        &self,
        force: bool,
        pty_manager: Option<&crate::pty::PtyManager>,
    ) -> Result<(), SessionError> {
        let _lifecycle = self.lifecycle_lock.lock().await;

        let current = self.get_state().await;
        if !current.can_stop() {
            return Ok(()); // Already stopped
        }

        if current == AgentState::Busy && !force {
            return Err(SessionError::Busy);
        }

        // Clear queue and current request with lock
        {
            let _queue_lock = self.request_queue_lock.lock().await;
            let mut queue = self.request_queue.lock().await;
            while let Some(req) = queue.pop_front() {
                let _ = req.response_tx.send(Err(SessionError::Stopped(
                    "Agent stopped by user".to_string(),
                )));
            }

            // Also clear current request
            let mut current_req = self.current_request.lock().await;
            if let Some(req) = current_req.take() {
                let _ = req.response_tx.send(Err(SessionError::Stopped(
                    "Agent stopped by user".to_string(),
                )));
            }
        }

        // Set state to dead
        *self.state.write().await = AgentState::Dead;

        // Clean up PTY from manager if provided
        if let Some(manager) = pty_manager {
            manager.remove(&self.name).await;
        }

        // Clean up PTY reference
        *self.pty.write().await = None;

        Ok(())
    }

    pub async fn interrupt(&self) -> Result<(), SessionError> {
        let _lifecycle = self.lifecycle_lock.lock().await;

        let current = self.get_state().await;
        if !current.can_interrupt() {
            return Ok(());
        }

        // Send interrupt sequence to PTY
        if let Some(pty) = self.pty.read().await.as_ref() {
            pty.write(self.adapter.get_interrupt_sequence())
                .await
                .map_err(|e| SessionError::PtyError(e.to_string()))?;
        }

        // Clear queue and current request with lock
        {
            let _queue_lock = self.request_queue_lock.lock().await;
            let mut queue = self.request_queue.lock().await;
            while let Some(req) = queue.pop_front() {
                let _ = req.response_tx.send(Err(SessionError::Stopped(
                    "Agent interrupted by user".to_string(),
                )));
            }

            // Also clear current request
            let mut current_req = self.current_request.lock().await;
            if let Some(req) = current_req.take() {
                let _ = req.response_tx.send(Err(SessionError::Stopped(
                    "Agent interrupted by user".to_string(),
                )));
            }

            // Reset state
            *self.state.write().await = AgentState::Idle;
        }

        Ok(())
    }

    pub async fn ask(
        self: &Arc<Self>,
        message: String,
        timeout: Option<Duration>,
        pty_manager: &crate::pty::PtyManager,
    ) -> Result<String, SessionError> {
        let timeout = timeout.unwrap_or(Duration::from_secs(self.timeouts.default));

        // Auto-start agent if stopped, with retry on failure
        let current = self.get_state().await;
        if !current.is_running() {
            self.start_with_retry(pty_manager).await?;
        }

        // Wait for agent to become ready (Idle or ReadyTimeout)
        let ready_timeout = Duration::from_secs(self.timeouts.ready_check);
        let ready_deadline = Instant::now() + ready_timeout;
        loop {
            let state = self.get_state().await;
            if state.can_accept_request() {
                break;
            }
            if !state.is_running() {
                return Err(SessionError::NotRunning);
            }
            if Instant::now() >= ready_deadline {
                return Err(SessionError::QueueTimeout);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        let (tx, rx) = oneshot::channel();
        let request = Request::new(message, timeout, tx);

        // Add to queue and prepare for processing under the lock
        let prepared = {
            let _queue_lock = self.request_queue_lock.lock().await;

            let current = self.get_state().await;
            if !current.is_running() {
                return Err(SessionError::NotRunning);
            }

            // Add to queue
            self.request_queue.lock().await.push_back(request);

            // If idle, prepare to process immediately
            if current.can_accept_request() {
                self.prepare_next_request().await
            } else {
                None
            }
        }; // Lock released here

        // Send to PTY and start detection (after releasing queue lock)
        if let Some(prepared) = prepared {
            let (result, should_retry) = self.send_prepared_request(prepared).await;
            if let Err(e) = result {
                tracing::warn!("Failed to send request: {}", e);
                // If should_retry, spawn a task to process next request in queue
                if should_retry {
                    let session = Arc::clone(self);
                    tokio::spawn(async move {
                        let _ = session.process_next_request().await;
                    });
                }
            }
        }

        // Wait for response with timeout
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(SessionError::Stopped("Channel closed".to_string())),
            Err(_) => Err(SessionError::RequestTimeout),
        }
    }

    /// Prepare the next request for processing. Must be called while holding queue_lock.
    /// Returns Some(prepared) if a request was prepared, None if queue empty or state not ready.
    async fn prepare_next_request(&self) -> Option<PreparedRequest> {
        // Peek at queue to see if there's a request
        let mut queue = self.request_queue.lock().await;
        if queue.is_empty() {
            return None;
        }

        // Try state transition first before popping
        let transition_result = self
            .apply_transition(StateTransition::AskAgent {
                message_id: "pending".to_string(), // Placeholder
            })
            .await;

        if transition_result.is_err() {
            // State transition failed, leave request in queue
            return None;
        }

        // Now safe to pop the request
        let request = queue.pop_front()?;
        drop(queue);

        let message_id = request.id.clone();
        let request_timeout = request.timeout;

        // Prepare message with sentinel
        let message_with_sentinel = self
            .adapter
            .inject_message_sentinel(&request.message, &message_id);

        // Lock session and get baseline offset for log detection
        let baseline_offset = if let Some(locked) = self.log_provider.lock_session().await {
            tracing::debug!(
                "Sending message pending to {}, locked session: {:?}",
                self.name,
                locked.file_path
            );
            locked.baseline_offset
        } else {
            tracing::debug!("Sending message pending to {}", self.name);
            self.log_provider.get_current_offset().await
        };

        // Get PTY offset for ClaudeCode parsing (before writing)
        let pty_start_offset = {
            let pty_guard = self.pty.read().await;
            if let Some(pty) = pty_guard.as_ref() {
                pty.get_current_offset().await
            } else {
                0
            }
        };

        // Store current request
        *self.current_request.lock().await = Some(request);

        Some(PreparedRequest {
            message_id,
            message_with_sentinel,
            baseline_offset,
            request_timeout,
            pty_start_offset,
        })
    }

    /// Send a prepared request to PTY and start reply detection.
    /// Called after releasing the queue lock to avoid blocking stop/interrupt.
    /// Returns (result, should_retry) - if should_retry is true, caller should try next request.
    async fn send_prepared_request(
        self: &Arc<Self>,
        prepared: PreparedRequest,
    ) -> (Result<(), SessionError>, bool) {
        // Check if PTY exists
        let pty_guard = self.pty.read().await;
        let Some(pty) = pty_guard.as_ref() else {
            // No PTY, unlock session and clear current request
            drop(pty_guard);
            self.log_provider.unlock_session().await;
            {
                let mut current_req = self.current_request.lock().await;
                if let Some(req) = current_req.take() {
                    let _ = req
                        .response_tx
                        .send(Err(SessionError::PtyError("No PTY available".to_string())));
                }
            }
            let _ = self.apply_transition(StateTransition::ReplyReceived).await;
            return (
                Err(SessionError::PtyError("No PTY available".to_string())),
                true, // should retry next request
            );
        };

        // Send message to PTY
        tracing::info!(
            "[Session] Sending message to {} PTY: {} bytes",
            self.name,
            prepared.message_with_sentinel.len()
        );
        if let Err(e) = pty.write_line(&prepared.message_with_sentinel).await {
            tracing::error!("[Session] PTY write failed for {}: {}", self.name, e);
            drop(pty_guard);
            // PTY write failed, unlock session and clear current request
            self.log_provider.unlock_session().await;
            {
                let mut current_req = self.current_request.lock().await;
                if let Some(req) = current_req.take() {
                    let _ = req
                        .response_tx
                        .send(Err(SessionError::PtyError(e.to_string())));
                }
            }
            // Try to go back to Idle (may fail if state changed)
            let _ = self.apply_transition(StateTransition::ReplyReceived).await;
            return (Err(SessionError::PtyError(e.to_string())), true);
        }
        drop(pty_guard);

        // Check if this is ClaudeCode agent (PTY-only parsing)
        let is_claudecode = self.adapter.as_any().is::<ClaudeCodeAgent>();

        if is_claudecode {
            // ClaudeCode: Use PTY parsing instead of LogProvider
            Self::spawn_claudecode_reply_detection(
                Arc::clone(self),
                prepared.message_id,
                prepared.pty_start_offset,
                prepared.request_timeout,
            );
        } else {
            // Other agents: Use LogProvider
            Self::spawn_reply_detection(
                Arc::clone(self),
                prepared.message_id,
                prepared.baseline_offset,
                prepared.request_timeout,
            );
        }

        (Ok(()), false)
    }

    async fn process_next_request(self: &Arc<Self>) -> Result<(), SessionError> {
        // Loop to handle errors and retry with next request
        loop {
            // Prepare under lock, then send after releasing
            let prepared = {
                let _queue_lock = self.request_queue_lock.lock().await;
                self.prepare_next_request().await
            };

            match prepared {
                Some(p) => {
                    let (result, should_retry) = self.send_prepared_request(p).await;
                    if should_retry {
                        // Error occurred, try next request
                        continue;
                    }
                    return result;
                }
                None => return Ok(()), // No more requests to process
            }
        }
    }

    fn spawn_reply_detection(
        session: Arc<Self>,
        _message_id: String,
        baseline_offset: u64,
        timeout: Duration,
    ) {
        let log_provider = session.log_provider.clone();
        let name = session.name.clone();

        tracing::info!(
            "[ReplyDetection] Starting for agent={}, baseline_offset={}, timeout={:?}",
            name,
            baseline_offset,
            timeout
        );

        tokio::spawn(async move {
            let deadline = Instant::now() + timeout;

            // Debounce interval in milliseconds
            const DEBOUNCE_MS: u64 = 100;
            // Fallback poll interval when file watching is unavailable
            const FALLBACK_POLL_MS: u64 = 2000;

            // Try to subscribe to file change events with debouncing
            let subscription = log_provider.subscribe_changes(DEBOUNCE_MS);
            tracing::debug!(
                "[ReplyDetection] File watcher subscription for {}: {}",
                name,
                if subscription.is_some() {
                    "active"
                } else {
                    "unavailable"
                }
            );

            // Initial check (handle changes that occurred before watcher started)
            tracing::debug!("[ReplyDetection] Performing initial check for {}", name);
            if let Some(entry) = log_provider.get_latest_reply(baseline_offset).await {
                tracing::info!(
                    "[ReplyDetection] Reply detected on initial check for {}: {} bytes",
                    name,
                    entry.content.len()
                );
                Self::deliver_reply(&session, entry).await;
                return;
            }
            tracing::debug!(
                "[ReplyDetection] No reply on initial check for {}, entering wait loop",
                name
            );

            match subscription {
                Some(mut sub) => {
                    // Event-driven mode with periodic polling as a safety net
                    tracing::debug!("[ReplyDetection] Using event-driven mode for {}", name);
                    let poll_interval = Duration::from_millis(FALLBACK_POLL_MS);
                    loop {
                        let remaining = deadline.saturating_duration_since(Instant::now());
                        if remaining.is_zero() {
                            tracing::warn!(
                                "[ReplyDetection] Timeout reached for {} in event mode",
                                name
                            );
                            drop(sub.handle); // Cancel watcher
                            Self::handle_reply_timeout(&session, &name).await;
                            return;
                        }

                        let wait = if remaining < poll_interval {
                            remaining
                        } else {
                            poll_interval
                        };

                        match tokio::time::timeout(wait, sub.receiver.recv()).await {
                            Ok(Ok(_event)) => {
                                tracing::debug!(
                                    "[ReplyDetection] File change event received for {}",
                                    name
                                );
                                // File changed, check for reply
                                if let Some(entry) =
                                    log_provider.get_latest_reply(baseline_offset).await
                                {
                                    tracing::info!(
                                        "[ReplyDetection] Reply detected for {}: {} bytes",
                                        name,
                                        entry.content.len()
                                    );
                                    drop(sub.handle); // Cancel watcher
                                    Self::deliver_reply(&session, entry).await;
                                    return;
                                }
                                tracing::debug!(
                                    "[ReplyDetection] No reply found after file change for {}",
                                    name
                                );
                            }
                            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(n))) => {
                                tracing::warn!(
                                    "[ReplyDetection] Missed {} events for {}, checking now",
                                    n,
                                    name
                                );
                                // Missed some events, check now
                                if let Some(entry) =
                                    log_provider.get_latest_reply(baseline_offset).await
                                {
                                    tracing::info!(
                                        "[ReplyDetection] Reply detected (lagged) for {}: {} bytes",
                                        name,
                                        entry.content.len()
                                    );
                                    drop(sub.handle);
                                    Self::deliver_reply(&session, entry).await;
                                    return;
                                }
                            }
                            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                                // Watcher closed, fall back to polling
                                tracing::warn!("[ReplyDetection] File watcher closed for {}, falling back to polling", name);
                                break;
                            }
                            Err(_) => {
                                // Periodic poll when no file events arrive
                                if let Some(entry) =
                                    log_provider.get_latest_reply(baseline_offset).await
                                {
                                    tracing::info!(
                                        "[ReplyDetection] Reply detected (poll) for {}: {} bytes",
                                        name,
                                        entry.content.len()
                                    );
                                    drop(sub.handle);
                                    Self::deliver_reply(&session, entry).await;
                                    return;
                                }
                                tracing::debug!(
                                    "[ReplyDetection] No reply found during periodic poll for {}",
                                    name
                                );
                            }
                        }
                    }
                }
                None => {
                    tracing::debug!(
                        "[ReplyDetection] File watching not available for {}, using fallback polling",
                        name
                    );
                }
            }

            // Fallback: polling mode (used when file watching is unavailable)
            tracing::debug!(
                "[ReplyDetection] Entering polling mode for {} with {}ms interval",
                name,
                FALLBACK_POLL_MS
            );
            let poll_interval = Duration::from_millis(FALLBACK_POLL_MS);
            let mut poll_count = 0u32;
            while Instant::now() < deadline {
                poll_count += 1;
                tracing::debug!("[ReplyDetection] Poll #{} for {}", poll_count, name);
                if let Some(entry) = log_provider.get_latest_reply(baseline_offset).await {
                    tracing::info!(
                        "[ReplyDetection] Reply detected (poll) for {}: {} bytes",
                        name,
                        entry.content.len()
                    );
                    Self::deliver_reply(&session, entry).await;
                    return;
                }
                tokio::time::sleep(poll_interval).await;
            }

            tracing::warn!(
                "[ReplyDetection] Final timeout after {} polls for {}",
                poll_count,
                name
            );
            Self::handle_reply_timeout(&session, &name).await;
        });
    }

    /// ClaudeCode-specific reply detection using PTY parsing
    fn spawn_claudecode_reply_detection(
        session: Arc<Self>,
        message_id: String,
        pty_start_offset: u64,
        timeout: Duration,
    ) {
        let name = session.name.clone();

        tokio::spawn(async move {
            // Get PTY handle
            let pty = {
                let pty_guard = session.pty.read().await;
                match pty_guard.as_ref() {
                    Some(p) => Arc::clone(p),
                    None => {
                        tracing::error!("No PTY available for ClaudeCode parsing");
                        Self::handle_reply_timeout(&session, &name).await;
                        return;
                    }
                }
            };

            // Downcast adapter to ClaudeCodeAgent
            let claudecode = match session.adapter.as_any().downcast_ref::<ClaudeCodeAgent>() {
                Some(cc) => cc,
                None => {
                    tracing::error!("Failed to downcast to ClaudeCodeAgent");
                    Self::handle_reply_timeout(&session, &name).await;
                    return;
                }
            };

            // Call parse_pty_response with per-request timeout
            match tokio::time::timeout(
                timeout,
                claudecode.parse_pty_response(&pty, pty_start_offset, &message_id),
            )
            .await
            {
                Ok(Ok(response)) => {
                    tracing::debug!("ClaudeCode reply detected for {}: {}", name, response);
                    // Create a LogEntry for compatibility with deliver_reply
                    let entry = crate::log_provider::LogEntry {
                        offset: pty_start_offset,
                        content: response,
                        timestamp: chrono::Utc::now(),
                        inode: None,
                    };
                    Self::deliver_reply(&session, entry).await;
                }
                Ok(Err(e)) => {
                    tracing::error!("ClaudeCode PTY parsing failed for {}: {}", name, e);
                    Self::deliver_reply_error(&session, SessionError::PtyError(e.to_string()))
                        .await;
                }
                Err(_) => {
                    tracing::warn!("ClaudeCode reply detection timed out for {}", name);
                    Self::handle_reply_timeout(&session, &name).await;
                }
            }
        });
    }

    async fn deliver_reply(session: &Arc<Self>, entry: crate::log_provider::LogEntry) {
        // Unlock session after reply detection completes
        session.log_provider.unlock_session().await;

        // Deliver result to waiting request
        {
            let mut current_req = session.current_request.lock().await;
            if let Some(req) = current_req.take() {
                let _ = req.response_tx.send(Ok(entry.content.clone()));
            }
        }

        // Apply state transition
        if let Err(e) = session
            .apply_transition(StateTransition::ReplyReceived)
            .await
        {
            tracing::warn!("Failed to apply ReplyReceived: {}", e);
        }

        // Process next request in queue
        let _ = session.process_next_request().await;
    }

    async fn deliver_reply_error(session: &Arc<Self>, error: SessionError) {
        // Unlock session after reply detection completes
        session.log_provider.unlock_session().await;

        {
            let mut current_req = session.current_request.lock().await;
            if let Some(req) = current_req.take() {
                let _ = req.response_tx.send(Err(error));
            }
        }

        if let Err(e) = session
            .apply_transition(StateTransition::ReplyReceived)
            .await
        {
            tracing::warn!("Failed to apply ReplyReceived: {}", e);
        }

        let _ = session.process_next_request().await;
    }

    async fn handle_reply_timeout(session: &Arc<Self>, name: &str) {
        // Unlock session after reply detection completes
        session.log_provider.unlock_session().await;

        tracing::warn!("Reply detection timed out for {}", name);

        // Best-effort cancel: send Ctrl+C (or agent-specific interrupt) so the session can accept
        // subsequent requests without mixing output from a still-running generation.
        let pty = {
            let pty_guard = session.pty.read().await;
            pty_guard.as_ref().cloned()
        };
        if let Some(pty) = pty {
            if let Err(e) = pty.write(session.adapter.get_interrupt_sequence()).await {
                tracing::warn!(
                    "Failed to send interrupt sequence after timeout for {}: {}",
                    name,
                    e
                );
            }
        }
        {
            let mut current_req = session.current_request.lock().await;
            if let Some(req) = current_req.take() {
                let _ = req.response_tx.send(Err(SessionError::RequestTimeout));
            }
        }

        if let Err(e) = session
            .apply_transition(StateTransition::RequestTimeout)
            .await
        {
            tracing::warn!("Failed to apply RequestTimeout: {}", e);
        }

        // Process next request in queue (critical: ensures queue is not blocked after timeout)
        let _ = session.process_next_request().await;
    }
}

pub struct SessionManager {
    sessions: RwLock<std::collections::HashMap<String, Arc<AgentSession>>>,
    pty_manager: Arc<crate::pty::PtyManager>,
}

impl SessionManager {
    pub fn new(pty_manager: Arc<crate::pty::PtyManager>) -> Self {
        Self {
            sessions: RwLock::new(std::collections::HashMap::new()),
            pty_manager,
        }
    }

    pub async fn register(&self, session: AgentSession) {
        let name = session.name.clone();
        self.sessions.write().await.insert(name, Arc::new(session));
    }

    pub async fn get(&self, name: &str) -> Option<Arc<AgentSession>> {
        self.sessions.read().await.get(name).cloned()
    }

    pub async fn list(&self) -> Vec<String> {
        self.sessions.read().await.keys().cloned().collect()
    }

    pub async fn get_all_status(&self) -> Vec<(String, AgentState)> {
        let sessions = self.sessions.read().await;
        let mut result = Vec::new();
        for (name, session) in sessions.iter() {
            let state = session.get_state().await;
            result.push((name.clone(), state));
        }
        result
    }

    pub fn pty_manager(&self) -> &Arc<crate::pty::PtyManager> {
        &self.pty_manager
    }

    /// Start all registered agents in parallel
    pub async fn start_all(&self) {
        let sessions: Vec<_> = self.sessions.read().await.values().cloned().collect();
        if sessions.is_empty() {
            return;
        }

        tracing::info!("Pre-starting {} agents...", sessions.len());

        let pty_manager = &self.pty_manager;
        let mut handles = Vec::with_capacity(sessions.len());

        for session in sessions {
            let pty_mgr = Arc::clone(pty_manager);
            handles.push(tokio::spawn(async move {
                let name = session.name.clone();
                match session.start_with_retry(&pty_mgr).await {
                    Ok(()) => tracing::info!("Agent {} started", name),
                    Err(e) => tracing::warn!("Failed to pre-start agent {}: {}", name, e),
                }
            }));
        }

        for handle in handles {
            let _ = handle.await;
        }

        tracing::info!("All agents pre-started");
    }

    /// Shutdown all sessions and their PTY processes
    pub async fn shutdown_all(&self) {
        tracing::info!("Shutting down all sessions...");

        // Stop all sessions first
        let sessions: Vec<_> = self.sessions.read().await.values().cloned().collect();
        for session in &sessions {
            tracing::debug!("Stopping session: {}", session.name);
            if let Err(e) = session.stop(true, Some(self.pty_manager.as_ref())).await {
                tracing::warn!("Failed to stop session {}: {}", session.name, e);
            }
        }

        // Then shutdown all PTY handles
        self.pty_manager.shutdown_all().await;

        tracing::info!("All sessions shut down");
    }
}
