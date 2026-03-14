use ccgonext::config::AgentConfig;
use ccgonext::events::EventLog;
#[cfg(windows)]
use ccgonext::mcp::{execute_tool, AskAgentsResponse};
use ccgonext::session::SessionManager;
use ccgonext::state::{ProcessState, TurnState};
#[cfg(windows)]
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
#[cfg(windows)]
use std::time::{Duration, Instant};

#[tokio::test]
async fn test_session_manager_get_or_create() {
    let mut configs = HashMap::new();
    configs.insert("codex".to_string(), AgentConfig::codex_default());
    let event_log = Arc::new(EventLog::new(100));

    let mgr = SessionManager::new(configs, event_log);

    let cwd = std::path::Path::new("/tmp");
    let session = mgr.get_or_create("codex", cwd).await;
    assert!(session.is_ok());

    let s1 = session.unwrap();
    let s2 = mgr.get_or_create("codex", cwd).await.unwrap();
    assert_eq!(s1.id, s2.id);
}

#[tokio::test]
async fn test_session_manager_unknown_agent() {
    let configs = HashMap::new();
    let event_log = Arc::new(EventLog::new(100));
    let mgr = SessionManager::new(configs, event_log);

    let result = mgr
        .get_or_create("unknown", std::path::Path::new("/tmp"))
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_session_manager_different_cwd_creates_new_session() {
    let mut configs = HashMap::new();
    configs.insert("codex".to_string(), AgentConfig::codex_default());
    let event_log = Arc::new(EventLog::new(100));

    let mgr = SessionManager::new(configs, event_log);

    let s1 = mgr
        .get_or_create("codex", std::path::Path::new("/tmp/a"))
        .await
        .unwrap();
    let s2 = mgr
        .get_or_create("codex", std::path::Path::new("/tmp/b"))
        .await
        .unwrap();

    assert_ne!(s1.id, s2.id);
}

#[tokio::test]
async fn test_session_manager_has_agent() {
    let mut configs = HashMap::new();
    configs.insert("codex".to_string(), AgentConfig::codex_default());
    let event_log = Arc::new(EventLog::new(100));

    let mgr = SessionManager::new(configs, event_log);
    assert!(mgr.has_agent("codex"));
    assert!(!mgr.has_agent("unknown"));
}

#[tokio::test]
async fn test_session_manager_get_or_create_is_atomic() {
    let mut configs = HashMap::new();
    configs.insert("codex".to_string(), AgentConfig::codex_default());
    let event_log = Arc::new(EventLog::new(100));
    let mgr = SessionManager::new(configs, event_log);
    let cwd = Arc::new(std::path::PathBuf::from("/tmp"));

    let mut tasks = Vec::new();
    for _ in 0..8 {
        let mgr = mgr.clone();
        let cwd = cwd.clone();
        tasks.push(tokio::spawn(async move {
            mgr.get_or_create("codex", &cwd).await.unwrap().id.clone()
        }));
    }

    let mut ids = Vec::new();
    for task in tasks {
        ids.push(task.await.unwrap());
    }

    let first = ids.first().unwrap().clone();
    assert!(ids.into_iter().all(|id| id == first));
}

#[tokio::test]
async fn test_shutdown_all_clears_session_indexes() {
    let mut configs = HashMap::new();
    configs.insert("codex".to_string(), AgentConfig::codex_default());
    let event_log = Arc::new(EventLog::new(100));
    let mgr = SessionManager::new(configs, event_log);

    let session = mgr
        .get_or_create("codex", std::path::Path::new("/tmp"))
        .await
        .unwrap();

    mgr.shutdown_all().await;

    assert!(mgr.get_by_id(&session.id).await.is_none());
    assert!(mgr.list_sessions().await.is_empty());
}

#[tokio::test]
async fn test_session_initial_state() {
    let mut configs = HashMap::new();
    configs.insert("codex".to_string(), AgentConfig::codex_default());
    let event_log = Arc::new(EventLog::new(100));
    let mgr = SessionManager::new(configs, event_log);

    let session = mgr
        .get_or_create("codex", std::path::Path::new("/tmp"))
        .await
        .unwrap();

    assert_eq!(session.process_state().await, ProcessState::Stopped);
    assert_eq!(session.turn_state().await, TurnState::Idle);
}

#[tokio::test]
async fn test_session_manager_get_by_id() {
    let mut configs = HashMap::new();
    configs.insert("codex".to_string(), AgentConfig::codex_default());
    let event_log = Arc::new(EventLog::new(100));
    let mgr = SessionManager::new(configs, event_log);

    let session = mgr
        .get_or_create("codex", std::path::Path::new("/tmp"))
        .await
        .unwrap();

    let found = mgr.get_by_id(&session.id).await;
    assert!(found.is_some());
    assert_eq!(found.unwrap().id, session.id);

    assert!(mgr.get_by_id("nonexistent-id").await.is_none());
}

#[tokio::test]
async fn test_session_manager_list_sessions() {
    let mut configs = HashMap::new();
    configs.insert("codex".to_string(), AgentConfig::codex_default());
    configs.insert("gemini".to_string(), AgentConfig::gemini_default());
    let event_log = Arc::new(EventLog::new(100));
    let mgr = SessionManager::new(configs, event_log);

    assert!(mgr.list_sessions().await.is_empty());

    mgr.get_or_create("codex", std::path::Path::new("/a"))
        .await
        .unwrap();
    mgr.get_or_create("gemini", std::path::Path::new("/b"))
        .await
        .unwrap();

    let sessions = mgr.list_sessions().await;
    assert_eq!(sessions.len(), 2);

    let agents: Vec<&str> = sessions.iter().map(|s| s.agent.as_str()).collect();
    assert!(agents.contains(&"codex"));
    assert!(agents.contains(&"gemini"));
}

#[tokio::test]
async fn test_session_manager_get_all_status() {
    let mut configs = HashMap::new();
    configs.insert("codex".to_string(), AgentConfig::codex_default());
    let event_log = Arc::new(EventLog::new(100));
    let mgr = SessionManager::new(configs, event_log);

    mgr.get_or_create("codex", std::path::Path::new("/tmp"))
        .await
        .unwrap();

    let statuses = mgr.get_all_status().await;
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].0, "codex");
    assert_eq!(statuses[0].1, ProcessState::Stopped);
    assert_eq!(statuses[0].2, TurnState::Idle);
}

#[cfg(windows)]
#[tokio::test]
async fn test_session_timeout_returns_promptly() {
    let temp = tempfile::tempdir().unwrap();
    let script_path = temp.path().join("fake-acp.ps1");
    std::fs::write(&script_path, fake_acp_timeout_script()).unwrap();

    let mut configs = HashMap::new();
    configs.insert(
        "codex".to_string(),
        AgentConfig::codex_default()
            .with_command("powershell".to_string())
            .with_args(vec![
                "-NoProfile".to_string(),
                "-ExecutionPolicy".to_string(),
                "Bypass".to_string(),
                "-File".to_string(),
                script_path.to_string_lossy().to_string(),
            ]),
    );

    let event_log = Arc::new(EventLog::new(100));
    let mgr = SessionManager::new(configs, event_log);
    let session = mgr.get_or_create("codex", temp.path()).await.unwrap();

    let started = Instant::now();
    let result = session
        .ask("slow request".to_string(), Some(Duration::from_secs(1)))
        .await;
    let elapsed = started.elapsed();

    assert!(result.is_err());
    assert_eq!(result.unwrap_err().to_string(), "Request timed out");
    assert!(
        elapsed < Duration::from_secs(3),
        "timeout returned too late: {:?}",
        elapsed
    );
    assert_eq!(session.process_state().await, ProcessState::Dead);
    assert_eq!(session.turn_state().await, TurnState::Idle);
}

#[cfg(windows)]
#[tokio::test]
async fn test_session_restarts_cleanly_after_timeout() {
    let temp = tempfile::tempdir().unwrap();
    let script_path = temp.path().join("fake-acp-restart.ps1");
    std::fs::write(&script_path, fake_acp_restart_script()).unwrap();

    let mut configs = HashMap::new();
    configs.insert(
        "codex".to_string(),
        AgentConfig::codex_default()
            .with_command("powershell".to_string())
            .with_args(vec![
                "-NoProfile".to_string(),
                "-ExecutionPolicy".to_string(),
                "Bypass".to_string(),
                "-File".to_string(),
                script_path.to_string_lossy().to_string(),
            ]),
    );

    let event_log = Arc::new(EventLog::new(100));
    let mgr = SessionManager::new(configs, event_log);
    let session = mgr.get_or_create("codex", temp.path()).await.unwrap();

    let first = session
        .ask("first".to_string(), Some(Duration::from_secs(1)))
        .await;
    assert!(first.is_err());
    assert_eq!(first.unwrap_err().to_string(), "Request timed out");
    assert_eq!(session.process_state().await, ProcessState::Dead);

    let second = session
        .ask("second".to_string(), Some(Duration::from_secs(3)))
        .await
        .unwrap();
    assert_eq!(second, "");
    assert_eq!(session.process_state().await, ProcessState::Running);
    assert_eq!(session.turn_state().await, TurnState::Idle);
}

#[cfg(windows)]
#[tokio::test]
async fn test_ask_agents_reports_timeout_in_result() {
    let temp = tempfile::tempdir().unwrap();
    let script_path = temp.path().join("fake-acp-timeout-mcp.ps1");
    std::fs::write(&script_path, fake_acp_timeout_script()).unwrap();

    let mut configs = HashMap::new();
    configs.insert(
        "codex".to_string(),
        AgentConfig::codex_default()
            .with_command("powershell".to_string())
            .with_args(vec![
                "-NoProfile".to_string(),
                "-ExecutionPolicy".to_string(),
                "Bypass".to_string(),
                "-File".to_string(),
                script_path.to_string_lossy().to_string(),
            ]),
    );

    let event_log = Arc::new(EventLog::new(100));
    let mgr = SessionManager::new(configs, event_log);

    let started = Instant::now();
    let response_json = execute_tool(
        "ask_agents",
        json!({
            "requests": [{"agent": "codex", "message": "slow"}],
            "timeout": 1,
            "project_root_path": temp.path().to_string_lossy().to_string(),
        }),
        &mgr,
    )
    .await
    .unwrap();
    let elapsed = started.elapsed();

    let response: AskAgentsResponse = serde_json::from_str(&response_json).unwrap();
    assert_eq!(response.results.len(), 1);
    assert!(!response.results[0].success);
    assert_eq!(
        response.results[0].error.as_deref(),
        Some("Request timed out")
    );
    assert!(
        elapsed < Duration::from_secs(3),
        "MCP timeout returned too late: {:?}",
        elapsed
    );
}

#[cfg(windows)]
fn fake_acp_timeout_script() -> &'static str {
    r#"
$ErrorActionPreference = 'Stop'
while (($line = [Console]::In.ReadLine()) -ne $null) {
    if ([string]::IsNullOrWhiteSpace($line)) { continue }
    $msg = $line | ConvertFrom-Json
    if ($msg.method -eq 'initialize') {
        $resp = @{
            jsonrpc = '2.0'
            id = $msg.id
            result = @{
                protocolVersion = 1
                agentCapabilities = @{}
                authMethods = @()
            }
        }
        [Console]::Out.WriteLine(($resp | ConvertTo-Json -Compress -Depth 10))
        continue
    }
    if ($msg.method -eq 'session/new') {
        $resp = @{
            jsonrpc = '2.0'
            id = $msg.id
            result = @{
                sessionId = 'fake-session'
            }
        }
        [Console]::Out.WriteLine(($resp | ConvertTo-Json -Compress -Depth 10))
        continue
    }
    if ($msg.method -eq 'session/prompt') {
        Start-Sleep -Seconds 10
        $resp = @{
            jsonrpc = '2.0'
            id = $msg.id
            result = @{
                stopReason = 'end_turn'
            }
        }
        [Console]::Out.WriteLine(($resp | ConvertTo-Json -Compress -Depth 10))
        continue
    }
}
"#
}

#[cfg(windows)]
fn fake_acp_restart_script() -> &'static str {
    r#"
$ErrorActionPreference = 'Stop'
$counterPath = Join-Path (Split-Path -Parent $MyInvocation.MyCommand.Path) 'prompt-counter.txt'
while (($line = [Console]::In.ReadLine()) -ne $null) {
    if ([string]::IsNullOrWhiteSpace($line)) { continue }
    $msg = $line | ConvertFrom-Json
    if ($msg.method -eq 'initialize') {
        $resp = @{
            jsonrpc = '2.0'
            id = $msg.id
            result = @{
                protocolVersion = 1
                agentCapabilities = @{}
                authMethods = @()
            }
        }
        [Console]::Out.WriteLine(($resp | ConvertTo-Json -Compress -Depth 10))
        continue
    }
    if ($msg.method -eq 'session/new') {
        $resp = @{
            jsonrpc = '2.0'
            id = $msg.id
            result = @{
                sessionId = 'fake-session'
            }
        }
        [Console]::Out.WriteLine(($resp | ConvertTo-Json -Compress -Depth 10))
        continue
    }
    if ($msg.method -eq 'session/prompt') {
        $count = 0
        if (Test-Path $counterPath) {
            $count = [int](Get-Content $counterPath -Raw)
        }
        Set-Content -Path $counterPath -Value ($count + 1)
        if ($count -eq 0) {
            Start-Sleep -Seconds 10
        }
        $resp = @{
            jsonrpc = '2.0'
            id = $msg.id
            result = @{
                stopReason = 'end_turn'
            }
        }
        [Console]::Out.WriteLine(($resp | ConvertTo-Json -Compress -Depth 10))
        continue
    }
}
"#
}
