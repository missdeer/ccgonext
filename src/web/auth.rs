//! Web authentication

use axum::{
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::Response,
};

use super::AppState;

const ALLOWED_HOSTS: &[&str] = &["localhost", "127.0.0.1", "::1"];

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

    let host_port = host_part.split('/').next().unwrap_or(host_part);

    // Split host:port (supports bracketed IPv6: [::1]:1234)
    let (host, port) = if let Some(host_port) = host_port.strip_prefix('[') {
        let Some(end) = host_port.find(']') else {
            return false;
        };
        let host = &host_port[..end];
        let port = host_port[end + 1..]
            .strip_prefix(':')
            .and_then(|p| p.parse::<u16>().ok());
        (host, port)
    } else {
        let (host, port) = match host_port.rsplit_once(':') {
            Some((host, port_str)) => (host, port_str.parse::<u16>().ok()),
            None => (host_port, None),
        };
        (host, port)
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
    let port = state.server_port;

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
