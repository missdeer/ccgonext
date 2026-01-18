//! Web authentication

use axum::{
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::Response,
};

use super::AppState;

const ALLOWED_HOSTS: &[&str] = &["localhost", "127.0.0.1"];

pub fn validate_origin(headers: &HeaderMap, allowed_port: u16) -> bool {
    let Some(origin) = headers.get("origin").and_then(|v| v.to_str().ok()) else {
        // No origin header - might be same-origin request
        return true;
    };

    // Simple parsing without url crate
    let origin = origin.trim();

    // Extract host part
    let host_part = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
        .unwrap_or(origin);

    // Split host:port
    let (host, port) = if let Some(colon_pos) = host_part.rfind(':') {
        let host = &host_part[..colon_pos];
        let port_str = &host_part[colon_pos + 1..];
        let port = port_str
            .split('/')
            .next()
            .and_then(|p| p.parse::<u16>().ok());
        (host, port)
    } else {
        (host_part.split('/').next().unwrap_or(host_part), None)
    };

    // Strict host matching
    if !ALLOWED_HOSTS.contains(&host) {
        return false;
    }

    // Check port if specified
    if let Some(port) = port {
        if port != allowed_port && port != 80 && port != 443 {
            return false;
        }
    }

    true
}

pub fn validate_bearer_token(headers: &HeaderMap, expected_token: &str) -> bool {
    let Some(auth) = headers.get("authorization").and_then(|v| v.to_str().ok()) else {
        return false;
    };

    if !auth.starts_with("Bearer ") {
        return false;
    }

    let token = &auth[7..];
    token == expected_token
}

pub async fn auth_middleware(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let port = state.config.server.port;

    // Validate origin for all requests
    if !validate_origin(&headers, port) {
        return Err(StatusCode::FORBIDDEN);
    }

    // Check bearer token if auth token is set
    if let Some(ref expected_token) = state.config.web.auth_token {
        if !validate_bearer_token(&headers, expected_token) {
            return Err(StatusCode::UNAUTHORIZED);
        }
    }

    Ok(next.run(request).await)
}
