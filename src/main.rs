use ccgonext::{
    acp::callbacks::CallbackPolicy,
    config::{AgentConfig, Config, ServerConfig, TimeoutConfig, WebConfig},
    events::EventLog,
    mcp::McpServer,
    session::SessionManager,
    web::{WebServer, WebServerRunOptions},
};
use clap::{Parser, Subcommand};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

#[derive(Parser)]
#[command(name = "ccgonext")]
#[command(author = "Claude Code Bridge Team")]
#[command(version)]
#[command(about = "ClaudeCode-Codex-Gemini-OpenCode Next MCP Server", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Web server port [env: CCGONEXT_PORT]
    #[arg(short, long, default_value = "8765", env = "CCGONEXT_PORT")]
    port: u16,

    /// Web server host [env: CCGONEXT_HOST]
    #[arg(long, default_value = "127.0.0.1", env = "CCGONEXT_HOST")]
    host: String,

    /// Retry binding to successive ports if the port is in use [env: CCGONEXT_PORT_RETRY]
    #[arg(long, default_value = "0", env = "CCGONEXT_PORT_RETRY")]
    port_retry: u16,

    /// Auto-open the web UI in a browser (web mode only) [env: CCGONEXT_OPEN_BROWSER]
    #[arg(long, env = "CCGONEXT_OPEN_BROWSER")]
    open_browser: bool,

    /// Auth token for web API [env: CCGONEXT_AUTH_TOKEN]
    #[arg(long, env = "CCGONEXT_AUTH_TOKEN")]
    auth_token: Option<String>,

    /// Default request timeout in seconds [env: CCGONEXT_TIMEOUT]
    #[arg(long, default_value = "600", env = "CCGONEXT_TIMEOUT")]
    timeout: u64,

    /// Codex command [env: CCGONEXT_CODEX_CMD]
    #[arg(long, default_value = "codex-acp", env = "CCGONEXT_CODEX_CMD")]
    codex_cmd: String,

    /// Gemini command [env: CCGONEXT_GEMINI_CMD]
    #[arg(long, default_value = "gemini", env = "CCGONEXT_GEMINI_CMD")]
    gemini_cmd: String,

    /// OpenCode command [env: CCGONEXT_OPENCODE_CMD]
    #[arg(long, default_value = "opencode", env = "CCGONEXT_OPENCODE_CMD")]
    opencode_cmd: String,

    /// ClaudeCode command [env: CCGONEXT_CLAUDECODE_CMD]
    #[arg(
        long,
        default_value = "claude-agent-acp",
        env = "CCGONEXT_CLAUDECODE_CMD"
    )]
    claudecode_cmd: String,

    /// Agents to enable (comma-separated: codex,gemini,opencode,claudecode) [env: CCGONEXT_AGENTS]
    #[arg(long, default_value = "codex,gemini,opencode", env = "CCGONEXT_AGENTS")]
    agents: String,

    /// Default callback policy for enabled agents [env: CCGONEXT_CALLBACK_POLICY]
    #[arg(long, env = "CCGONEXT_CALLBACK_POLICY", value_enum, default_value_t = CallbackPolicy::AutoApprove)]
    callback_policy: CallbackPolicy,

    /// Callback policy override for Codex [env: CCGONEXT_CODEX_CALLBACK_POLICY]
    #[arg(long, env = "CCGONEXT_CODEX_CALLBACK_POLICY", value_enum)]
    codex_callback_policy: Option<CallbackPolicy>,

    /// Callback policy override for Gemini [env: CCGONEXT_GEMINI_CALLBACK_POLICY]
    #[arg(long, env = "CCGONEXT_GEMINI_CALLBACK_POLICY", value_enum)]
    gemini_callback_policy: Option<CallbackPolicy>,

    /// Callback policy override for OpenCode [env: CCGONEXT_OPENCODE_CALLBACK_POLICY]
    #[arg(long, env = "CCGONEXT_OPENCODE_CALLBACK_POLICY", value_enum)]
    opencode_callback_policy: Option<CallbackPolicy>,

    /// Callback policy override for ClaudeCode [env: CCGONEXT_CLAUDECODE_CALLBACK_POLICY]
    #[arg(long, env = "CCGONEXT_CLAUDECODE_CALLBACK_POLICY", value_enum)]
    claudecode_callback_policy: Option<CallbackPolicy>,

    /// Idle timeout in seconds before auto-stopping agents [env: CCGONEXT_IDLE_TIMEOUT]
    #[arg(long, default_value = "900", env = "CCGONEXT_IDLE_TIMEOUT")]
    idle_timeout: u64,

    /// Log file path (optional) [env: CCGONEXT_LOG_FILE]
    #[arg(long, env = "CCGONEXT_LOG_FILE")]
    log_file: Option<String>,

    /// Log directory for rotating logs [env: CCGONEXT_LOG_DIR]
    #[arg(long, env = "CCGONEXT_LOG_DIR")]
    log_dir: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run as MCP server (stdio mode) with web UI
    Serve,
    /// Run web server only (standalone mode)
    Web,
    /// Show current configuration
    Config,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Hold the non-blocking log writer's worker guard alive for the lifetime
    // of main. Previously we `mem::forget`'d it, which meant the background
    // writer never flushed on shutdown — shutdown-path tracing was silently
    // discarded. Dropping the guard here at function return drains the queue.
    let _tracing_guard = init_tracing(&cli);

    let config = Arc::new(build_config(&cli));

    match cli.command {
        Some(Commands::Serve) | None => {
            run_mcp_server(config, cli.port_retry).await?;
        }
        Some(Commands::Web) => {
            run_web_server(config, cli.port_retry, cli.open_browser).await?;
        }
        Some(Commands::Config) => {
            show_config(&config);
        }
    }

    Ok(())
}

fn init_tracing(cli: &Cli) -> Option<WorkerGuard> {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "ccgonext=debug,tower_http=debug".into());

    let stderr_layer = fmt::layer().with_target(false).with_writer(std::io::stderr);

    if let Some(log_dir) = &cli.log_dir {
        let file_appender = tracing_appender::rolling::daily(log_dir, "ccgonext.log");
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

        let file_layer = fmt::layer()
            .with_target(true)
            .with_ansi(false)
            .with_writer(non_blocking);

        tracing_subscriber::registry()
            .with(env_filter)
            .with(stderr_layer)
            .with(file_layer)
            .init();

        return Some(guard);
    } else if let Some(log_file) = &cli.log_file {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_file)
            .expect("Failed to open log file");

        let file_layer = fmt::layer()
            .with_target(true)
            .with_ansi(false)
            .with_writer(std::sync::Arc::new(file));

        tracing_subscriber::registry()
            .with(env_filter)
            .with(stderr_layer)
            .with(file_layer)
            .init();
    } else {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(stderr_layer)
            .init();
    }
    None
}

fn build_config(cli: &Cli) -> Config {
    let enabled_agents: Vec<&str> = cli.agents.split(',').map(|s| s.trim()).collect();
    let project_root = std::env::current_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .to_string_lossy()
        .to_string();

    let idle_timeout = Duration::from_secs(cli.idle_timeout);

    let mut agents = HashMap::new();

    if enabled_agents.contains(&"codex") {
        agents.insert(
            "codex".to_string(),
            AgentConfig::codex_default()
                .with_command(cli.codex_cmd.clone())
                .with_callback_policy(resolve_callback_policy(
                    cli.codex_callback_policy.clone(),
                    &cli.callback_policy,
                ))
                .with_idle_timeout(idle_timeout),
        );
    }

    if enabled_agents.contains(&"gemini") {
        agents.insert(
            "gemini".to_string(),
            AgentConfig::gemini_default()
                .with_command(cli.gemini_cmd.clone())
                .with_callback_policy(resolve_callback_policy(
                    cli.gemini_callback_policy.clone(),
                    &cli.callback_policy,
                ))
                .with_idle_timeout(idle_timeout),
        );
    }

    if enabled_agents.contains(&"opencode") {
        agents.insert(
            "opencode".to_string(),
            AgentConfig::opencode_default()
                .with_command(cli.opencode_cmd.clone())
                .with_callback_policy(resolve_callback_policy(
                    cli.opencode_callback_policy.clone(),
                    &cli.callback_policy,
                ))
                .with_idle_timeout(idle_timeout),
        );
    }

    if enabled_agents.contains(&"claudecode") {
        agents.insert(
            "claudecode".to_string(),
            AgentConfig::claudecode_default()
                .with_command(cli.claudecode_cmd.clone())
                .with_callback_policy(resolve_callback_policy(
                    cli.claudecode_callback_policy.clone(),
                    &cli.callback_policy,
                ))
                .with_idle_timeout(idle_timeout),
        );
    }

    Config {
        server: ServerConfig {
            port: cli.port,
            host: cli.host.clone(),
        },
        agents,
        timeouts: TimeoutConfig {
            default: cli.timeout,
        },
        web: WebConfig {
            auth_token: cli.auth_token.clone(),
            project_root,
        },
    }
}

async fn run_mcp_server(config: Arc<Config>, port_retry: u16) -> anyhow::Result<()> {
    let session_manager = create_session_manager(&config);

    let web_config = config.clone();
    let web_session_manager = session_manager.clone();
    let web_handle = tokio::spawn(async move {
        let web_server = WebServer::new(web_session_manager, web_config);
        let options = WebServerRunOptions {
            port_retry,
            open_browser: false,
        };
        if let Err(e) = web_server.run_with_options(options).await {
            tracing::error!("Web server error: {}", e);
        }
    });

    let mcp_server = McpServer::new(session_manager.clone(), config);
    let result = tokio::select! {
        result = mcp_server.run_stdio() => result,
        _ = wait_for_shutdown_signal() => {
            tracing::info!("Received shutdown signal");
            Ok::<(), anyhow::Error>(())
        }
    };

    tracing::info!("Cleaning up sessions...");
    session_manager.shutdown_all().await;
    web_handle.abort();
    tracing::info!("Cleanup complete");

    result
}

async fn run_web_server(
    config: Arc<Config>,
    port_retry: u16,
    open_browser: bool,
) -> anyhow::Result<()> {
    let session_manager = create_session_manager(&config);
    let shutdown_manager = session_manager.clone();

    let web_server = WebServer::new(session_manager, config);
    let options = WebServerRunOptions {
        port_retry,
        open_browser,
    };
    let result = tokio::select! {
        result = web_server.run_with_options(options) => result,
        _ = wait_for_shutdown_signal() => {
            tracing::info!("Received shutdown signal");
            Ok::<(), anyhow::Error>(())
        }
    };

    tracing::info!("Cleaning up sessions...");
    shutdown_manager.shutdown_all().await;
    tracing::info!("Cleanup complete");

    result
}

async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = signal(SignalKind::terminate()).expect("Failed to register SIGTERM");
        let mut sigint = signal(SignalKind::interrupt()).expect("Failed to register SIGINT");
        let mut sighup = signal(SignalKind::hangup()).expect("Failed to register SIGHUP");

        tokio::select! {
            _ = sigterm.recv() => tracing::info!("Received SIGTERM"),
            _ = sigint.recv() => tracing::info!("Received SIGINT"),
            _ = sighup.recv() => tracing::info!("Received SIGHUP"),
        }
    }

    #[cfg(windows)]
    {
        use tokio::signal::ctrl_c;
        ctrl_c().await.expect("Failed to listen for Ctrl+C");
        tracing::info!("Received Ctrl+C");
    }
}

fn create_session_manager(config: &Config) -> Arc<SessionManager> {
    let event_log = Arc::new(EventLog::new(10_000));
    let working_dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    tracing::info!("Working directory for agents: {:?}", working_dir);

    SessionManager::new(config.agents.clone(), event_log)
}

fn resolve_callback_policy(
    override_policy: Option<CallbackPolicy>,
    default_policy: &CallbackPolicy,
) -> CallbackPolicy {
    override_policy.unwrap_or_else(|| default_policy.clone())
}

fn show_config(config: &Config) {
    println!("CCGONEXT Configuration");
    println!("==================");
    println!();
    println!("Server:");
    println!("  Host: {}", config.server.host);
    println!("  Port: {}", config.server.port);
    println!();
    println!("Web:");
    println!(
        "  Auth token: {}",
        if config.web.auth_token.is_some() {
            "set"
        } else {
            "none"
        }
    );
    println!();
    println!("Timeouts:");
    println!("  Default: {}s", config.timeouts.default);
    println!();
    println!("Agents:");
    for (name, agent_config) in &config.agents {
        println!(
            "  - {} (command: {} {:?}, callback_policy: {:?}, idle_timeout: {}s)",
            name,
            agent_config.acp_command,
            agent_config.acp_args,
            agent_config.callback_policy,
            agent_config.idle_timeout.as_secs()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn parse_cli(args: &[&str]) -> Cli {
        let argv = std::iter::once("ccgonext")
            .chain(args.iter().copied())
            .collect::<Vec<_>>();
        Cli::parse_from(argv)
    }

    #[test]
    fn test_build_config_uses_auto_approve_by_default() {
        let cli = parse_cli(&[]);
        let config = build_config(&cli);
        assert_eq!(
            config.agents.get("codex").unwrap().callback_policy,
            CallbackPolicy::AutoApprove
        );
        assert_eq!(
            config.agents.get("gemini").unwrap().callback_policy,
            CallbackPolicy::AutoApprove
        );
        assert_eq!(
            config.agents.get("opencode").unwrap().callback_policy,
            CallbackPolicy::AutoApprove
        );
    }

    #[test]
    fn test_build_config_applies_global_callback_policy() {
        let cli = parse_cli(&["--callback-policy", "deny-all"]);
        let config = build_config(&cli);
        assert_eq!(
            config.agents.get("codex").unwrap().callback_policy,
            CallbackPolicy::DenyAll
        );
        assert_eq!(
            config.agents.get("gemini").unwrap().callback_policy,
            CallbackPolicy::DenyAll
        );
    }

    #[test]
    fn test_build_config_agent_callback_policy_override_wins() {
        let cli = parse_cli(&[
            "--callback-policy",
            "deny-all",
            "--codex-callback-policy",
            "read-only",
        ]);
        let config = build_config(&cli);
        assert_eq!(
            config.agents.get("codex").unwrap().callback_policy,
            CallbackPolicy::ReadOnly
        );
        assert_eq!(
            config.agents.get("gemini").unwrap().callback_policy,
            CallbackPolicy::DenyAll
        );
    }
}
