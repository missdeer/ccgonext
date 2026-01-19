//! HTTP handlers

use super::AppState;
use axum::{extract::State, http::StatusCode, response::Json};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct AgentStatus {
    pub name: String,
    pub state: String,
}

#[derive(Debug, Serialize)]
pub struct StatusResponse {
    pub agents: Vec<AgentStatus>,
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

    Ok(Json(StatusResponse { agents }))
}
