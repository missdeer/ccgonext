//! Web service layer

mod auth;
mod handlers;
mod static_files;
mod websocket;

pub use auth::*;
pub use handlers::*;
pub use static_files::*;
pub use websocket::*;

use crate::config::Config;
use crate::session::SessionManager;
use axum::{
    middleware,
    routing::{get, post},
    Router,
};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};

#[derive(Debug, Clone, Copy)]
pub struct WebServerRunOptions {
    pub port_retry: u16,
    pub open_browser: bool,
}

impl Default for WebServerRunOptions {
    fn default() -> Self {
        Self {
            port_retry: 0,
            open_browser: false,
        }
    }
}

pub struct WebServer {
    session_manager: Arc<SessionManager>,
    config: Arc<Config>,
}

impl WebServer {
    pub fn new(session_manager: Arc<SessionManager>, config: Arc<Config>) -> Self {
        Self {
            session_manager,
            config,
        }
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        self.run_with_options(WebServerRunOptions::default()).await
    }

    pub async fn run_with_options(&self, options: WebServerRunOptions) -> anyhow::Result<()> {
        let session_manager = self.session_manager.clone();
        let config = self.config.clone();

        let host: IpAddr = config.server.host.parse()?;
        let base_port = config.server.port;
        let listener = bind_listener(host, base_port, options.port_retry).await?;

        // Port may differ if base_port was 0 (ephemeral) or due to retry.
        let server_port = listener.local_addr()?.port();

        let ui_addr = ui_addr_for_bind(host, server_port);
        let ui_url = format!("http://{}", ui_addr);
        tracing::info!("Web server bound to {}", listener.local_addr()?);
        tracing::info!("Web server UI available at {}", ui_url);

        if options.open_browser {
            if let Err(e) = open_browser(&ui_url) {
                tracing::warn!("Failed to open browser: {}", e);
            }
        }

        // Build CORS layer
        let cors = CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any);

        let state = AppState {
            session_manager,
            config,
            server_port,
        };

        // Build router with auth middleware
        let app = Router::new()
            .route("/api/status", get(api_get_status))
            .route("/api/restart/:agent", post(api_restart_agent))
            .route("/ws/:agent", get(ws_handler))
            .fallback(static_handler)
            .layer(middleware::from_fn_with_state(
                state.clone(),
                auth_middleware,
            ))
            .layer(cors)
            .with_state(state);

        axum::serve(listener, app).await?;

        Ok(())
    }
}

async fn bind_listener(
    host: IpAddr,
    base_port: u16,
    port_retry: u16,
) -> anyhow::Result<tokio::net::TcpListener> {
    let bind_port_once = base_port == 0 || port_retry == 0;
    if bind_port_once {
        let addr = SocketAddr::new(host, base_port);
        return Ok(tokio::net::TcpListener::bind(addr).await?);
    }

    let mut retries_left = port_retry;
    let mut port = base_port;
    loop {
        let addr = SocketAddr::new(host, port);
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => return Ok(listener),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse && retries_left > 0 => {
                if port == u16::MAX {
                    return Err(anyhow::anyhow!(
                        "Port {} is in use and cannot retry past u16::MAX",
                        port
                    ));
                }
                let next_port = port + 1;
                tracing::warn!("Port {} is in use, trying {}", port, next_port);
                port = next_port;
                retries_left -= 1;
            }
            Err(e) => return Err(e.into()),
        }
    }
}

fn ui_addr_for_bind(bind_host: IpAddr, port: u16) -> SocketAddr {
    match bind_host {
        IpAddr::V4(v4) if v4.is_unspecified() => {
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
        }
        IpAddr::V6(v6) if v6.is_unspecified() => {
            SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port)
        }
        _ => SocketAddr::new(bind_host, port),
    }
}

fn open_browser(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer").arg(url).spawn()?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(url).spawn()?;
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open").arg(url).spawn()?;
    }
    Ok(())
}

#[derive(Clone)]
pub struct AppState {
    pub session_manager: Arc<SessionManager>,
    pub config: Arc<Config>,
    pub server_port: u16,
}
