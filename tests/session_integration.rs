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
#[cfg(windows)]
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

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
    let prompt_file = temp.path().join("prompt-started.txt");

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

    let session_for_task = session.clone();
    let ask_handle = tokio::spawn(async move {
        session_for_task
            .ask("slow request".to_string(), Some(Duration::from_secs(1)))
            .await
    });
    assert!(
        wait_for_file(&prompt_file).await,
        "fake ACP never received session/prompt"
    );
    let started = Instant::now();
    let result = ask_handle.await.unwrap();
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
async fn test_session_timeout_kills_acp_process_tree() {
    let temp = tempfile::tempdir().unwrap();
    let script_path = temp.path().join("fake-acp-timeout-grandchild.ps1");
    std::fs::write(&script_path, fake_acp_timeout_grandchild_script()).unwrap();
    let pid_file = temp.path().join("grandchild-pid.txt");
    let self_pid_file = temp.path().join("self-pid.txt");

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

    let result = session
        .ask("slow request".to_string(), Some(Duration::from_secs(1)))
        .await;
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().to_string(), "Request timed out");

    let grandchild_pid = wait_for_pid_file(&pid_file)
        .await
        .expect("grandchild PID was never written");
    let fake_acp_pid = wait_for_pid_file(&self_pid_file)
        .await
        .expect("fake-acp PID was never written");

    let mut fake_acp_died_at: Option<u32> = None;
    let mut grandchild_died_at: Option<u32> = None;
    for tick in 0..60 {
        if fake_acp_died_at.is_none() && !is_process_alive(fake_acp_pid) {
            fake_acp_died_at = Some(tick);
        }
        if grandchild_died_at.is_none() && !is_process_alive(grandchild_pid) {
            grandchild_died_at = Some(tick);
        }
        if fake_acp_died_at.is_some() && grandchild_died_at.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    if fake_acp_died_at.is_none() {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &fake_acp_pid.to_string()])
            .output();
    }
    if grandchild_died_at.is_none() {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &grandchild_pid.to_string()])
            .output();
    }

    assert!(
        fake_acp_died_at.is_some(),
        "fake-acp PID {} survived after request timeout",
        fake_acp_pid
    );
    assert!(
        grandchild_died_at.is_some(),
        "grandchild PID {} survived after request timeout (fake-acp PID {} died at tick {:?})",
        grandchild_pid,
        fake_acp_pid,
        fake_acp_died_at
    );
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
    let prompt_file = temp.path().join("prompt-started.txt");

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

    let mgr_for_task = mgr.clone();
    let project_root_path = temp.path().to_string_lossy().to_string();
    let tool_handle = tokio::spawn(async move {
        execute_tool(
            "ask_agents",
            json!({
                "requests": [{"agent": "codex", "message": "slow"}],
                "timeout": 1,
                "project_root_path": project_root_path,
            }),
            &mgr_for_task,
        )
        .await
    });
    assert!(
        wait_for_file(&prompt_file).await,
        "fake ACP never received session/prompt"
    );
    let started = Instant::now();
    let response_json = tool_handle.await.unwrap().unwrap();
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
#[tokio::test]
async fn test_mcp_stdio_exit_kills_acp_process_tree() {
    let temp = tempfile::tempdir().unwrap();
    let script_path = temp.path().join("fake-acp-grandchild-server-exit.ps1");
    std::fs::write(&script_path, fake_acp_grandchild_with_wrapper_script()).unwrap();
    let powershell = powershell_exe();
    std::fs::copy(powershell, temp.path().join("node.exe")).unwrap();
    std::fs::copy(powershell, temp.path().join("codex-acp.exe")).unwrap();
    std::fs::write(
        temp.path().join("fake-node.ps1"),
        fake_node_wrapper_script(),
    )
    .unwrap();

    let shim_path = temp.path().join("fake-acp.cmd");
    std::fs::write(
        &shim_path,
        "@echo off\r\npowershell -NoProfile -ExecutionPolicy Bypass -File \"%~dp0fake-acp-grandchild-server-exit.ps1\"\r\n",
    )
    .unwrap();
    let pid_file = temp.path().join("grandchild-pid.txt");
    let self_pid_file = temp.path().join("self-pid.txt");
    let escaped_node_pid_file = temp.path().join("escaped-node-pid.txt");
    let escaped_codex_pid_file = temp.path().join("escaped-codex-pid.txt");

    let acp_command = shim_path.to_string_lossy().to_string();

    let mut server = tokio::process::Command::new(env!("CARGO_BIN_EXE_ccgonext"))
        .arg("--agents")
        .arg("codex")
        .arg("--codex-cmd")
        .arg(acp_command)
        .arg("--port")
        .arg("0")
        .arg("serve")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("failed to spawn ccgonext test server");

    let mut stdin = server.stdin.take().expect("server stdin should be piped");
    let stdout = server.stdout.take().expect("server stdout should be piped");
    let stderr = server.stderr.take().expect("server stderr should be piped");
    let mut stdout = tokio::io::BufReader::new(stdout);
    let stderr_task = tokio::spawn(async move {
        let mut stderr = stderr;
        let mut output = String::new();
        let _ = stderr.read_to_string(&mut output).await;
        output
    });

    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "ask_agents",
            "arguments": {
                "requests": [{"agent": "codex", "message": "hello"}],
                "timeout": 5,
                "project_root_path": temp.path().to_string_lossy(),
            }
        }
    });
    stdin
        .write_all(serde_json::to_string(&request).unwrap().as_bytes())
        .await
        .unwrap();
    stdin.write_all(b"\n").await.unwrap();
    stdin.flush().await.unwrap();

    let mut response_line = String::new();
    let bytes = tokio::time::timeout(
        Duration::from_secs(15),
        stdout.read_line(&mut response_line),
    )
    .await
    .expect("timed out waiting for MCP response")
    .expect("failed to read MCP response");
    assert!(bytes > 0, "ccgonext exited before writing MCP response");
    let response: serde_json::Value = serde_json::from_str(response_line.trim()).unwrap();
    assert_eq!(response["id"], 1);
    if response.get("error").is_some() {
        let (_, stderr) = stop_test_server(server, stdin, stderr_task).await;
        panic!(
            "MCP request failed before fake ACP could start: response={}, stderr={}",
            response_line.trim(),
            stderr
        );
    }

    let tool_text = response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("");
    let tool_response: serde_json::Value = serde_json::from_str(tool_text).unwrap_or_default();
    if tool_response["results"][0]["success"] != true {
        let (_, stderr) = stop_test_server(server, stdin, stderr_task).await;
        panic!(
            "ask_agents failed before fake ACP could start: response={}, stderr={}",
            tool_text, stderr
        );
    }

    let grandchild_pid = match wait_for_pid_file(&pid_file).await {
        Some(pid) => pid,
        None => {
            let (_, stderr) = stop_test_server(server, stdin, stderr_task).await;
            panic!(
                "grandchild PID was never written; response={}, stderr={}",
                tool_text, stderr
            );
        }
    };
    let fake_acp_pid = match wait_for_pid_file(&self_pid_file).await {
        Some(pid) => pid,
        None => {
            let (_, stderr) = stop_test_server(server, stdin, stderr_task).await;
            panic!(
                "fake-acp PID was never written; response={}, stderr={}",
                tool_text, stderr
            );
        }
    };
    let escaped_node_pid = match wait_for_pid_file(&escaped_node_pid_file).await {
        Some(pid) => pid,
        None => {
            let (_, stderr) = stop_test_server(server, stdin, stderr_task).await;
            panic!(
                "escaped node PID was never written; response={}, stderr={}",
                tool_text, stderr
            );
        }
    };
    let escaped_codex_pid = match wait_for_pid_file(&escaped_codex_pid_file).await {
        Some(pid) => pid,
        None => {
            let (_, stderr) = stop_test_server(server, stdin, stderr_task).await;
            panic!(
                "escaped codex-acp PID was never written; response={}, stderr={}",
                tool_text, stderr
            );
        }
    };
    assert!(
        is_process_alive(grandchild_pid),
        "grandchild PID {} should be alive before server exit",
        grandchild_pid
    );
    assert!(
        is_process_alive(escaped_node_pid),
        "escaped node PID {} should be alive before server exit",
        escaped_node_pid
    );
    assert!(
        is_process_alive(escaped_codex_pid),
        "escaped codex-acp PID {} should be alive before server exit",
        escaped_codex_pid
    );

    let (status, _stderr) = stop_test_server(server, stdin, stderr_task).await;
    let status = status.unwrap_or_else(|| panic!("ccgonext server did not exit after stdin EOF"));
    assert!(status.success(), "ccgonext exited with {}", status);

    let mut fake_acp_died_at: Option<u32> = None;
    let mut grandchild_died_at: Option<u32> = None;
    let mut escaped_node_died_at: Option<u32> = None;
    let mut escaped_codex_died_at: Option<u32> = None;
    for tick in 0..60 {
        if fake_acp_died_at.is_none() && !is_process_alive(fake_acp_pid) {
            fake_acp_died_at = Some(tick);
        }
        if grandchild_died_at.is_none() && !is_process_alive(grandchild_pid) {
            grandchild_died_at = Some(tick);
        }
        if escaped_node_died_at.is_none() && !is_process_alive(escaped_node_pid) {
            escaped_node_died_at = Some(tick);
        }
        if escaped_codex_died_at.is_none() && !is_process_alive(escaped_codex_pid) {
            escaped_codex_died_at = Some(tick);
        }
        if fake_acp_died_at.is_some()
            && grandchild_died_at.is_some()
            && escaped_node_died_at.is_some()
            && escaped_codex_died_at.is_some()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    if fake_acp_died_at.is_none() {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &fake_acp_pid.to_string()])
            .output();
    }
    if grandchild_died_at.is_none() {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &grandchild_pid.to_string()])
            .output();
    }
    if escaped_node_died_at.is_none() {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &escaped_node_pid.to_string()])
            .output();
    }
    if escaped_codex_died_at.is_none() {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &escaped_codex_pid.to_string()])
            .output();
    }

    assert!(
        fake_acp_died_at.is_some(),
        "fake-acp PID {} survived after ccgonext stdio exit",
        fake_acp_pid
    );
    assert!(
        grandchild_died_at.is_some(),
        "grandchild PID {} survived after ccgonext stdio exit (fake-acp PID {} died at tick {:?})",
        grandchild_pid,
        fake_acp_pid,
        fake_acp_died_at
    );
    assert!(
        escaped_node_died_at.is_some(),
        "escaped node PID {} survived after ccgonext stdio exit",
        escaped_node_pid
    );
    assert!(
        escaped_codex_died_at.is_some(),
        "escaped codex-acp PID {} survived after ccgonext stdio exit (escaped node PID {} died at tick {:?})",
        escaped_codex_pid,
        escaped_node_pid,
        escaped_node_died_at
    );
}

#[cfg(windows)]
async fn stop_test_server(
    mut server: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    stderr_task: tokio::task::JoinHandle<String>,
) -> (Option<std::process::ExitStatus>, String) {
    drop(stdin);

    let status = match tokio::time::timeout(Duration::from_secs(15), server.wait()).await {
        Ok(Ok(status)) => Some(status),
        Ok(Err(_)) => None,
        Err(_) => {
            let _ = server.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(5), server.wait()).await;
            None
        }
    };

    let stderr = match tokio::time::timeout(Duration::from_secs(5), stderr_task).await {
        Ok(Ok(output)) => output,
        _ => String::new(),
    };

    (status, stderr)
}

#[cfg(windows)]
fn powershell_exe() -> &'static str {
    "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe"
}

#[cfg(windows)]
fn fake_node_wrapper_script() -> &'static str {
    r#"
$ErrorActionPreference = 'Stop'
$dir = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Content -Path (Join-Path $dir 'escaped-node-pid.txt') -Value $PID
$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = (Join-Path $dir 'codex-acp.exe')
$codexPidPath = Join-Path $dir 'escaped-codex-pid.txt'
$psi.Arguments = "-NoProfile -ExecutionPolicy Bypass -Command `"Set-Content -Path '$codexPidPath' -Value `$PID; Start-Sleep -Seconds 120`""
$psi.UseShellExecute = $false
$psi.CreateNoWindow = $true
$codex = [System.Diagnostics.Process]::Start($psi)
$codex.WaitForExit()
"#
}

#[cfg(windows)]
fn fake_acp_grandchild_with_wrapper_script() -> &'static str {
    r#"
$ErrorActionPreference = 'Stop'
Add-Type -Namespace JC -Name P -MemberDefinition @"
[DllImport("kernel32.dll", SetLastError=true)] public static extern bool IsProcessInJob(IntPtr h, IntPtr j, out bool r);
[DllImport("kernel32.dll", SetLastError=true)] public static extern IntPtr OpenProcess(uint a, bool i, uint id);
[DllImport("kernel32.dll")] public static extern bool CloseHandle(IntPtr h);
"@
$dir = Split-Path -Parent $MyInvocation.MyCommand.Path
$nodePsi = New-Object System.Diagnostics.ProcessStartInfo
$nodePsi.FileName = (Join-Path $dir 'node.exe')
$nodePsi.Arguments = '-NoProfile -ExecutionPolicy Bypass -File "' + (Join-Path $dir 'fake-node.ps1') + '"'
$nodePsi.UseShellExecute = $false
$nodePsi.CreateNoWindow = $true
$node = [System.Diagnostics.Process]::Start($nodePsi)
$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = 'powershell'
$psi.Arguments = '-NoProfile -Command "Start-Sleep -Seconds 120"'
$psi.UseShellExecute = $false
$psi.CreateNoWindow = $true
$psi.RedirectStandardOutput = $true
$psi.RedirectStandardError = $true
$grandchild = [System.Diagnostics.Process]::Start($psi)
Set-Content -Path (Join-Path $dir 'grandchild-pid.txt') -Value $grandchild.Id
Set-Content -Path (Join-Path $dir 'self-pid.txt') -Value $PID
$selfInJob = $false
$gcInJob = $false
$selfH = [JC.P]::OpenProcess(0x400, $false, [uint32]$PID)
[JC.P]::IsProcessInJob($selfH, [IntPtr]::Zero, [ref]$selfInJob) | Out-Null
[JC.P]::CloseHandle($selfH) | Out-Null
$gcH = [JC.P]::OpenProcess(0x400, $false, [uint32]$grandchild.Id)
[JC.P]::IsProcessInJob($gcH, [IntPtr]::Zero, [ref]$gcInJob) | Out-Null
[JC.P]::CloseHandle($gcH) | Out-Null
Set-Content -Path (Join-Path $dir 'job-status.txt') -Value "self=$selfInJob;grandchild=$gcInJob"
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
        Set-Content -Path (Join-Path (Split-Path -Parent $MyInvocation.MyCommand.Path) 'prompt-started.txt') -Value $PID
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
fn fake_acp_timeout_grandchild_script() -> &'static str {
    r#"
$ErrorActionPreference = 'Stop'
$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = 'powershell'
$psi.Arguments = '-NoProfile -Command "Start-Sleep -Seconds 120"'
$psi.UseShellExecute = $false
$psi.CreateNoWindow = $true
$psi.RedirectStandardOutput = $true
$psi.RedirectStandardError = $true
$grandchild = [System.Diagnostics.Process]::Start($psi)
$dir = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Content -Path (Join-Path $dir 'grandchild-pid.txt') -Value $grandchild.Id
Set-Content -Path (Join-Path $dir 'self-pid.txt') -Value $PID
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
        Start-Sleep -Seconds 120
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

#[cfg(windows)]
#[tokio::test]
async fn test_session_shutdown_kills_grandchildren() {
    let temp = tempfile::tempdir().unwrap();
    let script_path = temp.path().join("fake-acp-grandchild.ps1");
    std::fs::write(&script_path, fake_acp_grandchild_script()).unwrap();
    let pid_file = temp.path().join("grandchild-pid.txt");
    let self_pid_file = temp.path().join("self-pid.txt");

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

    session
        .ask("hello".to_string(), Some(Duration::from_secs(5)))
        .await
        .expect("ask should succeed against fake acp");

    let mut grandchild_pid: Option<u32> = None;
    let mut fake_acp_pid: Option<u32> = None;
    for _ in 0..30 {
        if grandchild_pid.is_none() {
            if let Ok(content) = std::fs::read_to_string(&pid_file) {
                if let Ok(pid) = content.trim().parse::<u32>() {
                    grandchild_pid = Some(pid);
                }
            }
        }
        if fake_acp_pid.is_none() {
            if let Ok(content) = std::fs::read_to_string(&self_pid_file) {
                if let Ok(pid) = content.trim().parse::<u32>() {
                    fake_acp_pid = Some(pid);
                }
            }
        }
        if grandchild_pid.is_some() && fake_acp_pid.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let grandchild_pid = grandchild_pid.expect("grandchild PID was never written");
    let fake_acp_pid = fake_acp_pid.expect("fake-acp PID was never written");

    let job_status = std::fs::read_to_string(temp.path().join("job-status.txt"))
        .unwrap_or_else(|_| "(missing)".to_string());

    assert!(
        is_process_alive(grandchild_pid),
        "grandchild PID {} should be alive immediately after spawn",
        grandchild_pid
    );

    session.shutdown().await;
    drop(mgr);

    let mut fake_acp_died_at: Option<u32> = None;
    let mut grandchild_died_at: Option<u32> = None;
    for tick in 0..60 {
        if fake_acp_died_at.is_none() && !is_process_alive(fake_acp_pid) {
            fake_acp_died_at = Some(tick);
        }
        if grandchild_died_at.is_none() && !is_process_alive(grandchild_pid) {
            grandchild_died_at = Some(tick);
        }
        if grandchild_died_at.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    if grandchild_died_at.is_none() {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/PID", &grandchild_pid.to_string()])
            .output();
    }

    assert!(
        grandchild_died_at.is_some(),
        "grandchild PID {} survived after session shutdown - Job Object cleanup failed (fake-acp PID {} died at tick {:?}, job-status: {})",
        grandchild_pid,
        fake_acp_pid,
        fake_acp_died_at,
        job_status.trim()
    );
}

#[cfg(windows)]
#[tokio::test]
async fn test_dropping_session_kills_process_tree_without_explicit_shutdown() {
    let temp = tempfile::tempdir().unwrap();
    let script_path = temp.path().join("fake-acp-grandchild-drop.ps1");
    std::fs::write(&script_path, fake_acp_grandchild_script()).unwrap();
    let pid_file = temp.path().join("grandchild-pid.txt");
    let self_pid_file = temp.path().join("self-pid.txt");

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

    session
        .ask("hello".to_string(), Some(Duration::from_secs(5)))
        .await
        .expect("ask should succeed against fake acp");

    let mut grandchild_pid: Option<u32> = None;
    let mut fake_acp_pid: Option<u32> = None;
    for _ in 0..30 {
        if grandchild_pid.is_none() {
            if let Ok(content) = std::fs::read_to_string(&pid_file) {
                if let Ok(pid) = content.trim().parse::<u32>() {
                    grandchild_pid = Some(pid);
                }
            }
        }
        if fake_acp_pid.is_none() {
            if let Ok(content) = std::fs::read_to_string(&self_pid_file) {
                if let Ok(pid) = content.trim().parse::<u32>() {
                    fake_acp_pid = Some(pid);
                }
            }
        }
        if grandchild_pid.is_some() && fake_acp_pid.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let grandchild_pid = grandchild_pid.expect("grandchild PID was never written");
    let fake_acp_pid = fake_acp_pid.expect("fake-acp PID was never written");

    assert!(
        is_process_alive(grandchild_pid),
        "grandchild PID {} should be alive immediately after spawn",
        grandchild_pid
    );

    drop(mgr);
    drop(session);

    let mut grandchild_died_at: Option<u32> = None;
    for tick in 0..60 {
        if !is_process_alive(grandchild_pid) {
            grandchild_died_at = Some(tick);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    if grandchild_died_at.is_none() {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &grandchild_pid.to_string()])
            .output();
    }
    if is_process_alive(fake_acp_pid) {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &fake_acp_pid.to_string()])
            .output();
    }

    assert!(
        grandchild_died_at.is_some(),
        "grandchild PID {} survived after dropping session without explicit shutdown (fake-acp PID {})",
        grandchild_pid,
        fake_acp_pid
    );
}

#[cfg(windows)]
#[tokio::test]
async fn test_acp_exit_reaps_process_tree_without_session_shutdown() {
    let temp = tempfile::tempdir().unwrap();
    let script_path = temp.path().join("fake-acp-exit-after-prompt.ps1");
    std::fs::write(&script_path, fake_acp_exit_after_prompt_script()).unwrap();
    let pid_file = temp.path().join("grandchild-pid.txt");
    let self_pid_file = temp.path().join("self-pid.txt");

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

    session
        .ask("hello".to_string(), Some(Duration::from_secs(5)))
        .await
        .expect("ask should receive the final fake-acp response");

    let mut grandchild_pid: Option<u32> = None;
    let mut fake_acp_pid: Option<u32> = None;
    for _ in 0..30 {
        if grandchild_pid.is_none() {
            if let Ok(content) = std::fs::read_to_string(&pid_file) {
                if let Ok(pid) = content.trim().parse::<u32>() {
                    grandchild_pid = Some(pid);
                }
            }
        }
        if fake_acp_pid.is_none() {
            if let Ok(content) = std::fs::read_to_string(&self_pid_file) {
                if let Ok(pid) = content.trim().parse::<u32>() {
                    fake_acp_pid = Some(pid);
                }
            }
        }
        if grandchild_pid.is_some() && fake_acp_pid.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let grandchild_pid = grandchild_pid.expect("grandchild PID was never written");
    let fake_acp_pid = fake_acp_pid.expect("fake-acp PID was never written");

    let mut grandchild_died_at: Option<u32> = None;
    for tick in 0..60 {
        if !is_process_alive(grandchild_pid) {
            grandchild_died_at = Some(tick);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    if grandchild_died_at.is_none() {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/PID", &grandchild_pid.to_string()])
            .output();
    }

    assert!(
        grandchild_died_at.is_some(),
        "grandchild PID {} survived after fake-acp PID {} exited without explicit session shutdown",
        grandchild_pid,
        fake_acp_pid
    );
}

#[cfg(windows)]
fn is_process_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }

        let mut exit_code = 0;
        let ok = GetExitCodeProcess(handle, &mut exit_code);
        let _ = CloseHandle(handle);

        ok != 0 && exit_code == STILL_ACTIVE as u32
    }
}

#[cfg(windows)]
async fn wait_for_pid_file(path: &std::path::Path) -> Option<u32> {
    for _ in 0..100 {
        if let Ok(content) = std::fs::read_to_string(path) {
            if let Ok(pid) = content.trim().parse::<u32>() {
                return Some(pid);
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    None
}

#[cfg(windows)]
async fn wait_for_file(path: &std::path::Path) -> bool {
    for _ in 0..100 {
        if path.exists() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

#[cfg(windows)]
fn fake_acp_grandchild_script() -> &'static str {
    r#"
$ErrorActionPreference = 'Stop'
Add-Type -Namespace JC -Name P -MemberDefinition @"
[DllImport("kernel32.dll", SetLastError=true)] public static extern bool IsProcessInJob(IntPtr h, IntPtr j, out bool r);
[DllImport("kernel32.dll", SetLastError=true)] public static extern IntPtr OpenProcess(uint a, bool i, uint id);
[DllImport("kernel32.dll")] public static extern bool CloseHandle(IntPtr h);
"@
$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = 'powershell'
$psi.Arguments = '-NoProfile -Command "Start-Sleep -Seconds 120"'
$psi.UseShellExecute = $false
$psi.CreateNoWindow = $true
$psi.RedirectStandardOutput = $true
$psi.RedirectStandardError = $true
$grandchild = [System.Diagnostics.Process]::Start($psi)
$dir = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Content -Path (Join-Path $dir 'grandchild-pid.txt') -Value $grandchild.Id
Set-Content -Path (Join-Path $dir 'self-pid.txt') -Value $PID
$selfInJob = $false
$gcInJob = $false
$selfH = [JC.P]::OpenProcess(0x400, $false, [uint32]$PID)
[JC.P]::IsProcessInJob($selfH, [IntPtr]::Zero, [ref]$selfInJob) | Out-Null
[JC.P]::CloseHandle($selfH) | Out-Null
$gcH = [JC.P]::OpenProcess(0x400, $false, [uint32]$grandchild.Id)
[JC.P]::IsProcessInJob($gcH, [IntPtr]::Zero, [ref]$gcInJob) | Out-Null
[JC.P]::CloseHandle($gcH) | Out-Null
Set-Content -Path (Join-Path $dir 'job-status.txt') -Value "self=$selfInJob;grandchild=$gcInJob"
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
fn fake_acp_exit_after_prompt_script() -> &'static str {
    r#"
$ErrorActionPreference = 'Stop'
$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = 'powershell'
$psi.Arguments = '-NoProfile -Command "Start-Sleep -Seconds 120"'
$psi.UseShellExecute = $false
$psi.CreateNoWindow = $true
$psi.RedirectStandardOutput = $true
$psi.RedirectStandardError = $true
$grandchild = [System.Diagnostics.Process]::Start($psi)
$dir = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Content -Path (Join-Path $dir 'grandchild-pid.txt') -Value $grandchild.Id
Set-Content -Path (Join-Path $dir 'self-pid.txt') -Value $PID
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
        [Console]::Out.Flush()
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
        [Console]::Out.Flush()
        continue
    }
    if ($msg.method -eq 'session/prompt') {
        $resp = @{
            jsonrpc = '2.0'
            id = $msg.id
            result = @{
                stopReason = 'end_turn'
            }
        }
        [Console]::Out.WriteLine(($resp | ConvertTo-Json -Compress -Depth 10))
        [Console]::Out.Flush()
        exit 0
    }
}
"#
}
