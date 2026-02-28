//! CCGONEXT CLI - ClaudeCode-Codex-Gemini-OpenCode Next MCP Server

use ccgonext::{
    agent,
    config::{AgentConfig, Config, ServerConfig, TimeoutConfig, WebConfig},
    log_provider,
    mcp::McpServer,
    pty::PtyManager,
    session::{AgentSession, SessionManager},
    web::{WebServer, WebServerRunOptions},
};
use clap::{Parser, Subcommand};
use std::collections::HashMap;
use std::sync::Arc;
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

    /// Windows: delay between CR/LF in Enter key sequence [env: CCGONEXT_WINDOWS_ENTER_DELAY_MS]
    #[arg(long, default_value = "200", env = "CCGONEXT_WINDOWS_ENTER_DELAY_MS")]
    windows_enter_delay_ms: u64,

    /// Enable web terminal input [env: CCGONEXT_INPUT_ENABLED]
    #[arg(long, env = "CCGONEXT_INPUT_ENABLED")]
    input_enabled: bool,

    /// Auth token for web API [env: CCGONEXT_AUTH_TOKEN]
    #[arg(long, env = "CCGONEXT_AUTH_TOKEN")]
    auth_token: Option<String>,

    /// Output buffer size in bytes [env: CCGONEXT_BUFFER_SIZE]
    #[arg(long, default_value = "10485760", env = "CCGONEXT_BUFFER_SIZE")]
    buffer_size: usize,

    /// Default request timeout in seconds [env: CCGONEXT_TIMEOUT]
    #[arg(long, default_value = "600", env = "CCGONEXT_TIMEOUT")]
    timeout: u64,

    /// Codex command [env: CCGONEXT_CODEX_CMD]
    #[arg(long, default_value = "codex", env = "CCGONEXT_CODEX_CMD")]
    codex_cmd: String,

    /// Gemini command [env: CCGONEXT_GEMINI_CMD]
    #[arg(long, default_value = "gemini", env = "CCGONEXT_GEMINI_CMD")]
    gemini_cmd: String,

    /// OpenCode command [env: CCGONEXT_OPENCODE_CMD]
    #[arg(long, default_value = "opencode", env = "CCGONEXT_OPENCODE_CMD")]
    opencode_cmd: String,

    /// ClaudeCode command [env: CCGONEXT_CLAUDECODE_CMD]
    #[arg(long, default_value = "claude", env = "CCGONEXT_CLAUDECODE_CMD")]
    claudecode_cmd: String,

    /// Agents to enable (comma-separated: codex,gemini,opencode,claudecode) [env: CCGONEXT_AGENTS]
    #[arg(long, default_value = "codex,gemini,opencode", env = "CCGONEXT_AGENTS")]
    agents: String,

    /// Maximum number of retries when agent fails to start [env: CCGONEXT_MAX_START_RETRIES]
    #[arg(long, default_value = "3", env = "CCGONEXT_MAX_START_RETRIES")]
    max_start_retries: u32,

    /// Base delay in milliseconds for exponential backoff between retries [env: CCGONEXT_START_RETRY_DELAY]
    #[arg(long, default_value = "1000", env = "CCGONEXT_START_RETRY_DELAY")]
    start_retry_delay: u64,

    /// Log file path (optional, if not set logs only go to stderr) [env: CCGONEXT_LOG_FILE]
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

    // Initialize tracing with optional file output
    init_tracing(&cli);

    let config = Arc::new(build_config(&cli));

    match cli.command {
        Some(Commands::Serve) | None => {
            run_mcp_server(config, cli.port_retry, cli.windows_enter_delay_ms).await?;
        }
        Some(Commands::Web) => {
            run_web_server(
                config,
                cli.port_retry,
                cli.open_browser,
                cli.windows_enter_delay_ms,
            )
            .await?;
        }
        Some(Commands::Config) => {
            show_config(&config);
        }
    }

    Ok(())
}

fn init_tracing(cli: &Cli) {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "ccgonext=debug,tower_http=debug".into());

    let stderr_layer = fmt::layer().with_target(false).with_writer(std::io::stderr);

    if let Some(log_dir) = &cli.log_dir {
        // Rotating file appender (daily rotation)
        let file_appender = tracing_appender::rolling::daily(log_dir, "ccgonext.log");
        let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

        let file_layer = fmt::layer()
            .with_target(true)
            .with_ansi(false)
            .with_writer(non_blocking);

        tracing_subscriber::registry()
            .with(env_filter)
            .with(stderr_layer)
            .with(file_layer)
            .init();

        // Leak the guard to keep it alive for the program lifetime
        std::mem::forget(_guard);
    } else if let Some(log_file) = &cli.log_file {
        // Single file appender
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
        // Stderr only
        tracing_subscriber::registry()
            .with(env_filter)
            .with(stderr_layer)
            .init();
    }
}

fn build_config(cli: &Cli) -> Config {
    let enabled_agents: Vec<&str> = cli.agents.split(',').map(|s| s.trim()).collect();
    let project_root = std::env::current_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .to_string_lossy()
        .to_string();

    let mut agents = HashMap::new();

    if enabled_agents.contains(&"codex") {
        agents.insert(
            "codex".to_string(),
            AgentConfig::codex_default().with_command(cli.codex_cmd.clone()),
        );
    }

    if enabled_agents.contains(&"gemini") {
        agents.insert(
            "gemini".to_string(),
            AgentConfig::gemini_default().with_command(cli.gemini_cmd.clone()),
        );
    }

    if enabled_agents.contains(&"opencode") {
        agents.insert(
            "opencode".to_string(),
            AgentConfig::opencode_default().with_command(cli.opencode_cmd.clone()),
        );
    }

    if enabled_agents.contains(&"claudecode") {
        agents.insert(
            "claudecode".to_string(),
            AgentConfig::claudecode_default().with_command(cli.claudecode_cmd.clone()),
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
            max_start_retries: cli.max_start_retries,
            start_retry_delay_ms: cli.start_retry_delay,
            ..TimeoutConfig::default()
        },
        web: WebConfig {
            auth_token: cli.auth_token.clone(),
            input_enabled: cli.input_enabled,
            output_buffer_size: cli.buffer_size,
            project_root,
        },
    }
}

async fn run_mcp_server(
    config: Arc<Config>,
    port_retry: u16,
    windows_enter_delay_ms: u64,
) -> anyhow::Result<()> {
    let session_manager = create_session_manager(&config, windows_enter_delay_ms).await?;

    // Set up shutdown signal handler
    let shutdown_manager = session_manager.clone();
    let shutdown_handle = tokio::spawn(async move {
        wait_for_shutdown_signal().await;
        tracing::info!("Received shutdown signal, cleaning up...");
        shutdown_manager.shutdown_all().await;
        tracing::info!("Cleanup complete");
    });

    // Start web server in background
    let web_config = config.clone();
    let web_session_manager = session_manager.clone();
    tokio::spawn(async move {
        let web_server = WebServer::new(web_session_manager, web_config);
        let options = WebServerRunOptions {
            port_retry,
            open_browser: false,
        };
        if let Err(e) = web_server.run_with_options(options).await {
            tracing::error!("Web server error: {}", e);
        }
    });

    // Run MCP server on stdio
    let mcp_server = McpServer::new(session_manager.clone(), config);
    let result = mcp_server.run_stdio().await;

    // Ensure cleanup happens even if MCP server exits normally
    session_manager.shutdown_all().await;
    shutdown_handle.abort();

    result
}

async fn run_web_server(
    config: Arc<Config>,
    port_retry: u16,
    open_browser: bool,
    windows_enter_delay_ms: u64,
) -> anyhow::Result<()> {
    let session_manager = create_session_manager(&config, windows_enter_delay_ms).await?;

    // Set up shutdown signal handler
    let shutdown_manager = session_manager.clone();
    tokio::spawn(async move {
        wait_for_shutdown_signal().await;
        tracing::info!("Received shutdown signal, cleaning up...");
        shutdown_manager.shutdown_all().await;
        tracing::info!("Cleanup complete");
        std::process::exit(0);
    });

    let web_server = WebServer::new(session_manager, config);
    let options = WebServerRunOptions {
        port_retry,
        open_browser,
    };
    web_server.run_with_options(options).await?;
    Ok(())
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

async fn create_session_manager(
    config: &Config,
    windows_enter_delay_ms: u64,
) -> anyhow::Result<Arc<SessionManager>> {
    let pty_manager = Arc::new(PtyManager::new_with_windows_enter_delay_ms(
        config.web.output_buffer_size,
        windows_enter_delay_ms,
    ));
    let session_manager = Arc::new(SessionManager::new(pty_manager));

    let working_dir = std::env::current_dir()?;
    tracing::info!("Working directory for agents: {:?}", working_dir);

    // Register configured agents
    for (name, agent_config) in &config.agents {
        let adapter = agent::create_agent(name, agent_config);

        // Create config with working_dir for LogProvider
        let mut log_config = std::collections::HashMap::new();
        log_config.insert(
            "working_dir".to_string(),
            working_dir.to_string_lossy().to_string(),
        );

        let log_provider: Arc<dyn log_provider::LogProvider> = Arc::from(
            log_provider::create_log_provider(&agent_config.log_provider, Some(&log_config)),
        );

        let session = AgentSession::new(
            name.clone(),
            Arc::from(adapter),
            log_provider,
            working_dir.clone(),
            config.timeouts.clone(),
        );

        session_manager.register(session).await;
    }

    // Pre-start all agents in background (non-blocking)
    // Use tokio::task::yield_now to ensure the spawn gets a chance to start
    let sm = session_manager.clone();
    tokio::spawn(async move {
        sm.start_all().await;
    });

    // Yield to allow the spawned task to start executing
    tokio::task::yield_now().await;

    Ok(session_manager)
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
    println!("  Input enabled: {}", config.web.input_enabled);
    println!(
        "  Auth token: {}",
        if config.web.auth_token.is_some() {
            "set"
        } else {
            "none"
        }
    );
    println!("  Buffer size: {} bytes", config.web.output_buffer_size);
    println!();
    println!("Timeouts:");
    println!("  Default: {}s", config.timeouts.default);
    println!("  Startup: {}s", config.timeouts.startup);
    println!("  Max start retries: {}", config.timeouts.max_start_retries);
    println!(
        "  Start retry delay: {}ms",
        config.timeouts.start_retry_delay_ms
    );
    println!();
    println!("Agents:");
    for (name, agent_config) in &config.agents {
        println!("  - {} (command: {})", name, agent_config.command);
    }
}
