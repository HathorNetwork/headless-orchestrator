use axum::body::Body;
use axum::http::{Response, StatusCode};
use reqwest::Method;

use crate::state::SharedState;

/// Proxy a request to the wallet-headless instance for the given session.
pub async fn proxy_request(
    state: &SharedState,
    session_id: &str,
    method: Method,
    path: &str,
    body: Option<String>,
    headers: &[(String, String)],
) -> Result<Response<Body>, (StatusCode, String)> {
    // Look up instance and update last_activity
    let (port, container_id, api_key) = {
        let mut instances = state.instances.write().await;
        let instance = instances
            .get_mut(session_id)
            .ok_or((StatusCode::NOT_FOUND, "Session not found".to_string()))?;
        instance.last_activity = std::time::Instant::now();
        (instance.port, instance.container_id.clone(), instance.api_key.clone())
    };

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

    // Inject the API key for this container
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
