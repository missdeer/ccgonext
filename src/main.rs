//! CCGO CLI - ClaudeCode-Codex-Gemini-OpenCode MCP Server

use ccgo::{
    agent,
    config::{AgentConfig, Config, ServerConfig, TimeoutConfig, WebConfig},
    log_provider,
    mcp::McpServer,
    pty::PtyManager,
    session::{AgentSession, SessionManager},
    web::WebServer,
};
use clap::{Parser, Subcommand};
use std::collections::HashMap;
use std::sync::Arc;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser)]
#[command(name = "ccgo")]
#[command(author = "Claude Code Bridge Team")]
#[command(version)]
#[command(about = "ClaudeCode-Codex-Gemini-OpenCode MCP Server", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Web server port [env: CCGO_PORT]
    #[arg(short, long, default_value = "8765", env = "CCGO_PORT")]
    port: u16,

    /// Web server host [env: CCGO_HOST]
    #[arg(long, default_value = "127.0.0.1", env = "CCGO_HOST")]
    host: String,

    /// Enable web terminal input [env: CCGO_INPUT_ENABLED]
    #[arg(long, env = "CCGO_INPUT_ENABLED")]
    input_enabled: bool,

    /// Auth token for web API [env: CCGO_AUTH_TOKEN]
    #[arg(long, env = "CCGO_AUTH_TOKEN")]
    auth_token: Option<String>,

    /// Output buffer size in bytes [env: CCGO_BUFFER_SIZE]
    #[arg(long, default_value = "10485760", env = "CCGO_BUFFER_SIZE")]
    buffer_size: usize,

    /// Default request timeout in seconds [env: CCGO_TIMEOUT]
    #[arg(long, default_value = "600", env = "CCGO_TIMEOUT")]
    timeout: u64,

    /// Codex command [env: CCGO_CODEX_CMD]
    #[arg(long, default_value = "codex", env = "CCGO_CODEX_CMD")]
    codex_cmd: String,

    /// Gemini command [env: CCGO_GEMINI_CMD]
    #[arg(long, default_value = "gemini", env = "CCGO_GEMINI_CMD")]
    gemini_cmd: String,

    /// OpenCode command [env: CCGO_OPENCODE_CMD]
    #[arg(long, default_value = "opencode", env = "CCGO_OPENCODE_CMD")]
    opencode_cmd: String,

    /// ClaudeCode command [env: CCGO_CLAUDECODE_CMD]
    #[arg(long, default_value = "claude", env = "CCGO_CLAUDECODE_CMD")]
    claudecode_cmd: String,

    /// Agents to enable (comma-separated: codex,gemini,opencode,claudecode) [env: CCGO_AGENTS]
    #[arg(long, default_value = "codex,gemini,opencode", env = "CCGO_AGENTS")]
    agents: String,

    /// Maximum number of retries when agent fails to start [env: CCGO_MAX_START_RETRIES]
    #[arg(long, default_value = "3", env = "CCGO_MAX_START_RETRIES")]
    max_start_retries: u32,

    /// Base delay in milliseconds for exponential backoff between retries [env: CCGO_START_RETRY_DELAY]
    #[arg(long, default_value = "1000", env = "CCGO_START_RETRY_DELAY")]
    start_retry_delay: u64,
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
    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ccgo=info".into()),
        )
        .with(tracing_subscriber::fmt::layer().with_target(false))
        .init();

    let cli = Cli::parse();
    let config = Arc::new(build_config(&cli));

    match cli.command {
        Some(Commands::Serve) | None => {
            run_mcp_server(config).await?;
        }
        Some(Commands::Web) => {
            run_web_server(config).await?;
        }
        Some(Commands::Config) => {
            show_config(&config);
        }
    }

    Ok(())
}

fn build_config(cli: &Cli) -> Config {
    let enabled_agents: Vec<&str> = cli.agents.split(',').map(|s| s.trim()).collect();

    let mut agents = HashMap::new();

    if enabled_agents.contains(&"codex") {
        agents.insert(
            "codex".to_string(),
            AgentConfig {
                command: cli.codex_cmd.clone(),
                args: vec![],
                log_provider: "codex".to_string(),
                ready_pattern: r"^(>|codex>)".to_string(),
                error_patterns: vec!["Error:".to_string(), "Traceback".to_string()],
                supports_cwd: true,
                sentinel_template: "# MSG_ID:{id}\n{message}".to_string(),
                sentinel_regex: r"# MSG_ID:([a-f0-9-]+)".to_string(),
            },
        );
    }

    if enabled_agents.contains(&"gemini") {
        agents.insert(
            "gemini".to_string(),
            AgentConfig {
                command: cli.gemini_cmd.clone(),
                args: vec![],
                log_provider: "gemini".to_string(),
                ready_pattern: r"(Gemini|>\s*$)".to_string(),
                error_patterns: vec!["Error:".to_string(), "Failed".to_string()],
                supports_cwd: true,
                sentinel_template: "\u{200B}MSG_ID:{id}\u{200B}\n{message}".to_string(),
                sentinel_regex: r"\u{200B}MSG_ID:([a-f0-9-]+)\u{200B}".to_string(),
            },
        );
    }

    if enabled_agents.contains(&"opencode") {
        agents.insert(
            "opencode".to_string(),
            AgentConfig {
                command: cli.opencode_cmd.clone(),
                args: vec![],
                log_provider: "opencode".to_string(),
                ready_pattern: r"(opencode|>\s*$)".to_string(),
                error_patterns: vec!["ERROR".to_string(), "Exception".to_string()],
                supports_cwd: true,
                sentinel_template: "[[MSG:{id}]]\n{message}".to_string(),
                sentinel_regex: r"\[\[MSG:([a-f0-9-]+)\]\]".to_string(),
            },
        );
    }

    if enabled_agents.contains(&"claudecode") {
        agents.insert(
            "claudecode".to_string(),
            AgentConfig {
                command: cli.claudecode_cmd.clone(),
                args: vec![],
                log_provider: "pty".to_string(),
                ready_pattern: r"(?m)^>\s*$".to_string(),
                error_patterns: vec![
                    r"(?i)^error:".to_string(),
                    r"(?i)^failed".to_string(),
                    r"(?i)^fatal".to_string(),
                ],
                supports_cwd: false,
                sentinel_template: "# CCGO_MSG_ID:{id}\n{message}".to_string(),
                sentinel_regex: r"(?i)#\s*CCGO_MSG_ID:\s*([0-9a-f-]{36})".to_string(),
            },
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
        },
    }
}

async fn run_mcp_server(config: Arc<Config>) -> anyhow::Result<()> {
    let session_manager = create_session_manager(&config).await?;

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
        if let Err(e) = web_server.run().await {
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

async fn run_web_server(config: Arc<Config>) -> anyhow::Result<()> {
    let session_manager = create_session_manager(&config).await?;

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
    web_server.run().await?;
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

async fn create_session_manager(config: &Config) -> anyhow::Result<Arc<SessionManager>> {
    let pty_manager = Arc::new(PtyManager::new(config.web.output_buffer_size));
    let session_manager = Arc::new(SessionManager::new(pty_manager));

    // Register configured agents
    for (name, agent_config) in &config.agents {
        let adapter = agent::create_agent(name, agent_config);

        let log_provider: Arc<dyn log_provider::LogProvider> = Arc::from(
            log_provider::create_log_provider(&agent_config.log_provider, None),
        );

        let session = AgentSession::new(
            name.clone(),
            Arc::from(adapter),
            log_provider,
            std::env::current_dir()?,
            config.timeouts.clone(),
        );

        session_manager.register(session).await;
    }

    Ok(session_manager)
}

fn show_config(config: &Config) {
    println!("CCGO Configuration");
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
