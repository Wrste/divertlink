//! Admin endpoints: health, readiness, Prometheus metrics and JSON status.
//!
//! Served on `server.admin_listen` (loopback by default) and never exposed through
//! the public ingress.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};

use super::ServerState;

/// Build the admin router.
pub fn router(state: Arc<ServerState>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics))
        .route("/status", get(status))
        .with_state(state)
}

/// Liveness: the process is up.
async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, "ok\n")
}

/// Readiness: at least one agent is connected, or the gateway only serves static
/// routes (which are always ready).
async fn readyz(State(state): State<Arc<ServerState>>) -> Response {
    let metrics = &state.metrics;
    let tunnels = state.registry.tunnels();
    let connected = tunnels.iter().filter(|t| t.is_connected()).count();
    let ready = tunnels.is_empty() || connected > 0;
    let body = Json(serde_json::json!({
        "ready": ready,
        "tunnels": tunnels.len(),
        "connected": connected,
        "connections_active": metrics.connections_active.load(Ordering::Relaxed),
        "uptime_seconds": metrics.uptime().as_secs(),
    }));
    if ready {
        (StatusCode::OK, body).into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, body).into_response()
    }
}

/// Prometheus text exposition.
async fn metrics(State(state): State<Arc<ServerState>>) -> Response {
    let body = state.metrics.render_prometheus();
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
        .into_response()
}

/// Full JSON status: tunnels, sessions and counters.
async fn status(State(state): State<Arc<ServerState>>) -> Response {
    let mut status = state.registry.status_json();
    if let Some(object) = status.as_object_mut() {
        object.insert(
            "server".into(),
            serde_json::json!({
                "name": state.config.server.name,
                "tunnel_listen": state.config.server.tunnel_listen.map(|a| a.to_string()),
                "ingress_listen": state.config.server.ingress_listen.map(|a| a.to_string()),
                "http_listen": state.config.server.http_listen.map(|a| a.to_string()),
                "admin_listen": state.config.server.admin_listen.map(|a| a.to_string()),
                "authentication": state.keys.is_enabled(),
                "api_keys": state.keys.len(),
            }),
        );
    }
    Json(status).into_response()
}
