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
use axum::{middleware, routing::get, Router};
use std::net::SocketAddr;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};

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
        let addr: SocketAddr =
            format!("{}:{}", self.config.server.host, self.config.server.port).parse()?;

        let session_manager = self.session_manager.clone();
        let config = self.config.clone();

        // Build CORS layer
        let cors = CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any);

        let state = AppState {
            session_manager,
            config,
        };

        // Build router with auth middleware
        let app = Router::new()
            .route("/api/status", get(api_get_status))
            .route("/ws/:agent", get(ws_handler))
            .fallback(static_handler)
            .layer(middleware::from_fn_with_state(
                state.clone(),
                auth_middleware,
            ))
            .layer(cors)
            .with_state(state);

        tracing::info!("Web server listening on http://{}", addr);

        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, app).await?;

        Ok(())
    }
}

#[derive(Clone)]
pub struct AppState {
    pub session_manager: Arc<SessionManager>,
    pub config: Arc<Config>,
}
