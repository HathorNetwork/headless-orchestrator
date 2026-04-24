use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Json, Response},
    routing::{any, delete, get, post},
    Router,
};
use serde_json::json;
use tower_http::cors::CorsLayer;
use uuid::Uuid;

use crate::docker;
use crate::proxy;
use crate::state::SharedState;

pub fn create_router(state: SharedState) -> Router {
    Router::new()
        // Session management
        .route("/sessions", post(create_session))
        .route("/sessions", get(list_sessions))
        .route("/sessions/:session_id", delete(destroy_session))
        // Proxy: all wallet-headless API calls go through here
        .route("/sessions/:session_id/api/*rest", any(proxy_handler))
        // Health
        .route("/health", get(health))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

/// POST /sessions — Spawn a new wallet-headless instance.
///
/// Returns `{ session_id, api_key, message }`. **The caller MUST store the
/// returned `api_key`** — it is required on every subsequent request to
/// `/sessions/:session_id/api/*` and on `DELETE /sessions/:session_id`. The
/// orchestrator does not persist it anywhere the caller can retrieve it
/// later; if lost, the session is unreachable and must be destroyed & recreated.
async fn create_session(State(state): State<SharedState>) -> impl IntoResponse {
    // Check capacity
    {
        let instances = state.instances.read().await;
        if state.max_instances > 0 && instances.len() >= state.max_instances {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": "Maximum number of instances reached"})),
            )
                .into_response();
        }
    }

    let session_id = Uuid::new_v4().to_string();

    match docker::spawn_instance(&state, &session_id).await {
        Ok(instance) => {
            let api_key = instance.api_key.clone();
            let mut instances = state.instances.write().await;
            instances.insert(session_id.clone(), instance);

            Json(json!({
                "session_id": session_id,
                "api_key": api_key,
                "message": "Wallet-headless instance is ready. Store the api_key — it is required on every request to this session and is not recoverable.",
            }))
            .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e})),
        )
            .into_response(),
    }
}

/// GET /sessions — List all active sessions.
async fn list_sessions(State(state): State<SharedState>) -> impl IntoResponse {
    let instances = state.instances.read().await;
    let sessions: Vec<_> = instances
        .values()
        .map(|inst| {
            json!({
                "session_id": inst.session_id,
                "port": inst.port,
                "idle_secs": inst.last_activity.elapsed().as_secs(),
            })
        })
        .collect();

    Json(json!({ "sessions": sessions }))
}

/// DELETE /sessions/:session_id — Destroy a wallet-headless instance.
///
/// Requires the caller to present the session's `x-api-key`. Without it (or
/// with a mismatched key) the orchestrator returns 401, so anyone who only
/// knows a session_id (e.g. via `GET /sessions`) cannot destroy other callers'
/// sessions.
async fn destroy_session(
    State(state): State<SharedState>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let caller_key = headers
        .get("x-api-key")
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());

    if let Err((status, msg)) =
        proxy::authorize_session(&state, &session_id, caller_key.as_deref()).await
    {
        return (status, Json(json!({"error": msg}))).into_response();
    }

    let removed = {
        let mut instances = state.instances.write().await;
        instances.remove(&session_id)
    };

    match removed {
        Some(_) => {
            let _ = docker::remove_instance(&session_id).await;
            Json(json!({"destroyed": true, "session_id": session_id})).into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Session not found"})),
        )
            .into_response(),
    }
}

/// ANY /sessions/:session_id/api/* — Proxy to the wallet-headless container.
///
/// Requires the caller's `x-api-key` to match the session's stored key.
/// Mismatched/missing keys are rejected with 401 at the proxy edge.
async fn proxy_handler(
    State(state): State<SharedState>,
    Path((session_id, rest)): Path<(String, String)>,
    Query(query_params): Query<std::collections::HashMap<String, String>>,
    method: Method,
    headers: HeaderMap,
    body: String,
) -> Response<Body> {
    let path = if query_params.is_empty() {
        format!("/{}", rest)
    } else {
        let qs: Vec<String> = query_params
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect();
        format!("/{}?{}", rest, qs.join("&"))
    };

    // Extract the caller-provided api key. Stripped from forwarded headers —
    // the proxy forwards the stored key itself (see proxy::proxy_request).
    let caller_api_key = headers
        .get("x-api-key")
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());

    // Forward relevant headers (excluding x-api-key — see above)
    let fwd_headers: Vec<(String, String)> = headers
        .iter()
        .filter(|(name, _)| {
            let n = name.as_str().to_lowercase();
            n == "x-wallet-id" || n == "content-type" || n == "accept"
        })
        .map(|(name, value)| {
            (
                name.to_string(),
                value.to_str().unwrap_or("").to_string(),
            )
        })
        .collect();

    let body_opt = if body.is_empty() { None } else { Some(body) };

    match proxy::proxy_request(
        &state,
        &session_id,
        caller_api_key.as_deref(),
        method,
        &path,
        body_opt,
        &fwd_headers,
    )
    .await
    {
        Ok(resp) => resp,
        Err((status, msg)) => Response::builder()
            .status(status)
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"error": msg}).to_string(),
            ))
            .unwrap(),
    }
}

/// GET /health
async fn health() -> impl IntoResponse {
    (StatusCode::OK, "OK")
}
