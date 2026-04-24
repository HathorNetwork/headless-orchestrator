use axum::body::Body;
use axum::http::{Response, StatusCode};
use reqwest::Method;

use crate::state::SharedState;

/// Look up the session's api_key and validate the caller presented a matching one.
///
/// Returns the instance's `(port, container_id, api_key)` on success, or a
/// `(StatusCode, message)` error tuple on failure. Also refreshes last_activity
/// when the key matches (no activity bump for unauthenticated callers).
async fn authorize(
    state: &SharedState,
    session_id: &str,
    caller_api_key: Option<&str>,
) -> Result<(u16, String, String), (StatusCode, String)> {
    let mut instances = state.instances.write().await;
    let instance = instances
        .get_mut(session_id)
        .ok_or((StatusCode::NOT_FOUND, "Session not found".to_string()))?;

    let Some(caller) = caller_api_key else {
        return Err((
            StatusCode::UNAUTHORIZED,
            "Missing x-api-key header".to_string(),
        ));
    };

    if caller != instance.api_key {
        return Err((StatusCode::UNAUTHORIZED, "Invalid api key".to_string()));
    }

    instance.last_activity = std::time::Instant::now();
    Ok((
        instance.port,
        instance.container_id.clone(),
        instance.api_key.clone(),
    ))
}

/// Validate the caller's api_key matches the session's stored key.
/// Used by non-proxy endpoints (e.g., DELETE /sessions/:id) that still need
/// to gate on per-session ownership but don't proxy to the container.
pub async fn authorize_session(
    state: &SharedState,
    session_id: &str,
    caller_api_key: Option<&str>,
) -> Result<(), (StatusCode, String)> {
    authorize(state, session_id, caller_api_key).await.map(|_| ())
}

/// Proxy a request to the wallet-headless instance for the given session.
///
/// Requires `caller_api_key` to match the session's stored api_key. The
/// orchestrator validates at its edge and also forwards the key to
/// wallet-headless (which enforces it independently) for defense in depth.
pub async fn proxy_request(
    state: &SharedState,
    session_id: &str,
    caller_api_key: Option<&str>,
    method: Method,
    path: &str,
    body: Option<String>,
    headers: &[(String, String)],
) -> Result<Response<Body>, (StatusCode, String)> {
    let (port, container_id, api_key) = authorize(state, session_id, caller_api_key).await?;

    // When attached to a custom docker network, reach the container by name on
    // its internal port (8000) — avoids needing host-published ports to be
    // reachable from the orchestrator's container. Otherwise fall back to
    // proxy_host:host_port.
    let url = if state.docker_network.is_some() {
        format!("http://{}:8000{}", container_id, path)
    } else {
        format!("http://{}:{}{}", state.proxy_host, port, path)
    };

    let client = &state.http_client;
    let mut req = client.request(method, &url);

    // Forward the validated api_key to wallet-headless. Using the stored
    // instance key (not the caller's raw header value) avoids re-echoing
    // attacker-controlled input and keeps a single source of truth.
    req = req.header("x-api-key", &api_key);

    for (key, value) in headers {
        req = req.header(key, value);
    }

    if let Some(body) = body {
        req = req.header("content-type", "application/json");
        req = req.body(body);
    }

    let resp = req
        .send()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("Upstream error: {}", e)))?;

    let status = StatusCode::from_u16(resp.status().as_u16())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let resp_body = resp
        .text()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("Body read error: {}", e)))?;

    Ok(Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(resp_body))
        .unwrap())
}
