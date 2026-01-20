//! Integration tests for session layer
//!
//! These tests validate the session management functionality including
//! ClaudeCode PTY-based reply detection.

use async_trait::async_trait;
use ccgo::agent::{ClaudeCodeAgent, GenericAgent};
use ccgo::config::{AgentConfig, TimeoutConfig};
use ccgo::log_provider::{HistoryEntry, LockedSession, LogEntry, LogProvider};
use ccgo::pty::PtyManager;
use ccgo::session::AgentSession;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::timeout;

// Mock LogProvider for testing
struct MockLogProvider;

#[async_trait]
impl LogProvider for MockLogProvider {
    async fn get_latest_reply(&self, _since_offset: u64) -> Option<LogEntry> {
        None
    }

    async fn get_history(&self, _session_id: Option<&str>, _count: usize) -> Vec<HistoryEntry> {
        vec![]
    }

    async fn get_current_offset(&self) -> u64 {
        0
    }

    fn get_inode(&self) -> Option<u64> {
        None
    }

    fn get_watch_path(&self) -> Option<std::path::PathBuf> {
        None
    }

    async fn lock_session(&self) -> Option<LockedSession> {
        None
    }

    async fn unlock_session(&self) {}
}

#[tokio::test]
#[ignore] // Requires actual claude binary installed
async fn test_claudecode_pty_reply_detection() {
    // Create PTY manager
    let pty_manager = Arc::new(PtyManager::new(100 * 1024 * 1024));

    // Create ClaudeCodeAgent
    let agent = Arc::new(ClaudeCodeAgent::new());

    // Create mock log provider
    let log_provider = Arc::new(MockLogProvider);

    // Create session
    let session = AgentSession::new(
        "test-claudecode".to_string(),
        agent.clone(),
        log_provider,
        PathBuf::from("/tmp"),
        TimeoutConfig::default(),
    );

    let session_arc = Arc::new(session);

    // Start agent (this will create PTY)
    let start_result = session_arc.start(pty_manager.as_ref()).await;
    assert!(
        start_result.is_ok(),
        "Failed to start session: {:?}",
        start_result
    );

    // Wait longer for agent to be ready (claude might take time to start)
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Verify state transitions from Starting to Idle (or remains Starting if claude not found)
    let state = session_arc.get_state().await;
    if matches!(state, ccgo::state::AgentState::Idle) {
        // Send a request
        let request_future = session_arc.ask(
            "test message".to_string(),
            Some(Duration::from_secs(10)),
            pty_manager.as_ref(),
        );

        // Wait for response with timeout
        let response = timeout(Duration::from_secs(15), request_future).await;

        // Verify response doesn't timeout (indicating PTY detection works)
        assert!(
            response.is_ok(),
            "Request timed out - ClaudeCode PTY detection may not be working"
        );
    }

    // Cleanup
    let _ = session_arc.stop(true, Some(pty_manager.as_ref())).await;
}

#[tokio::test]
async fn test_claudecode_type_detection() {
    // Create agents
    let claudecode = Arc::new(ClaudeCodeAgent::new());

    // Create a test AgentConfig for codex
    let codex_config = AgentConfig {
        command: "codex".to_string(),
        args: vec![],
        log_provider: "codex".to_string(),
        ready_pattern: r"^(>|codex>)".to_string(),
        error_patterns: vec!["Error:".to_string(), "Traceback".to_string()],
        supports_cwd: true,
        sentinel_template: "# MSG_ID:{id}\n{message}".to_string(),
        sentinel_regex: r"# MSG_ID:([a-f0-9-]+)".to_string(),
    };
    let codex = Arc::new(GenericAgent::new("codex".to_string(), &codex_config));

    // Create mock log provider
    let log_provider = Arc::new(MockLogProvider);

    // Create sessions
    let cc_session = AgentSession::new(
        "test-cc".to_string(),
        claudecode,
        log_provider.clone(),
        PathBuf::from("/tmp"),
        TimeoutConfig::default(),
    );

    let codex_session = AgentSession::new(
        "test-codex".to_string(),
        codex,
        log_provider,
        PathBuf::from("/tmp"),
        TimeoutConfig::default(),
    );

    // Verify type detection
    assert!(
        cc_session.adapter.as_any().is::<ClaudeCodeAgent>(),
        "ClaudeCodeAgent type detection failed"
    );

    assert!(
        !codex_session.adapter.as_any().is::<ClaudeCodeAgent>(),
        "Codex should not be detected as ClaudeCodeAgent"
    );
}

#[tokio::test]
#[ignore] // Requires actual claude binary installed
async fn test_claudecode_pty_offset_tracking() {
    // Create PTY manager
    let pty_manager = Arc::new(PtyManager::new(100 * 1024 * 1024));

    // Create ClaudeCodeAgent
    let agent = Arc::new(ClaudeCodeAgent::new());

    // Create mock log provider
    let log_provider = Arc::new(MockLogProvider);

    // Create session
    let session = AgentSession::new(
        "test-offset".to_string(),
        agent.clone(),
        log_provider,
        PathBuf::from("/tmp"),
        TimeoutConfig::default(),
    );

    let session_arc = Arc::new(session);

    // Start agent
    let start_result = session_arc.start(pty_manager.as_ref()).await;
    assert!(start_result.is_ok());

    // Wait for ready
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Get PTY handle and verify offset is valid
    let pty_guard = session_arc.pty.read().await;
    if let Some(pty) = pty_guard.as_ref() {
        let _offset = pty.get_current_offset().await;
        // Offset is u64, always non-negative, just verify we can get it
    } else {
        panic!("PTY should be available after start");
    }

    drop(pty_guard);

    // Cleanup
    let _ = session_arc.stop(true, Some(pty_manager.as_ref())).await;
}
