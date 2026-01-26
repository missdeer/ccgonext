//! HTTP handlers

use super::AppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct AgentStatus {
    pub name: String,
    pub state: String,
}

#[derive(Debug, Serialize)]
pub struct StatusResponse {
    pub agents: Vec<AgentStatus>,
    pub input_enabled: bool,
}

pub async fn api_get_status(
    State(state): State<AppState>,
) -> Result<Json<StatusResponse>, StatusCode> {
    let statuses = state.session_manager.get_all_status().await;

    let agents = statuses
        .into_iter()
        .map(|(name, s)| AgentStatus {
            name,
            state: s.to_string(),
        })
        .collect();

    Ok(Json(StatusResponse {
        agents,
        input_enabled: state.config.web.input_enabled,
    }))
}

#[derive(Debug, Serialize)]
pub struct RestartResponse {
    pub success: bool,
    pub message: String,
}

pub async fn api_restart_agent(
    State(state): State<AppState>,
    Path(agent): Path<String>,
) -> Result<Json<RestartResponse>, StatusCode> {
    let session = state
        .session_manager
        .get(&agent)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;

    // Stop the agent (force=true to stop even if busy)
    if let Err(e) = session
        .stop(true, Some(state.session_manager.pty_manager().as_ref()))
        .await
    {
        return Ok(Json(RestartResponse {
            success: false,
            message: format!("Failed to stop agent: {}", e),
        }));
    }

    // Start the agent with retry
    if let Err(e) = session
        .start_with_retry(state.session_manager.pty_manager())
        .await
    {
        return Ok(Json(RestartResponse {
            success: false,
            message: format!("Failed to start agent: {}", e),
        }));
    }

    Ok(Json(RestartResponse {
        success: true,
        message: format!("Agent {} restarted successfully", agent),
    }))
}
