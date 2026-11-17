//! Public HTTPS ingress: authentication, routing, forwarding through tunnels or
//! to static upstreams (new-api, dashboards, ...).

use std::net::IpAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::header::{self, HeaderValue};
use axum::http::{Request, Response as HttpResponse, StatusCode};
use axum::Router;
use bytes::Bytes;
use http_body_util::BodyExt;
use hyper::body::Incoming;
use tokio::sync::OwnedSemaphorePermit;
use tracing::{debug, warn};

use super::ServerState;
use crate::auth::{client_ip, AuthError, KeyGuard, PeerAddr};
use crate::config::UpstreamAuth;
use crate::error::Error;
use crate::httpfwd::{
    error_response, matches_public_path, new_request_id, sanitize_request_headers,
    sanitize_response_headers, set_retry_after, ForwardCtx,
};
use crate::metrics::TunnelStats;
use crate::registry::{ActiveSession, Resolved, StaticRoute, TunnelEntry};
use crate::util::is_safe_path;

/// Build the ingress router (also used for the plaintext listener).
pub fn router(state: Arc<ServerState>) -> Router {
    Router::new().fallback(ingress_handler).with_state(state)
}

pub async fn ingress_handler(
    State(state): State<Arc<ServerState>>,
    request: Request<Body>,
) -> HttpResponse<Body> {
    let request_id = new_request_id();
    let started = std::time::Instant::now();
    let peer_ip = request
        .extensions()
        .get::<PeerAddr>()
        .map(|p| p.0)
        .unwrap_or_else(|| IpAddr::from([127, 0, 0, 1]));
    let ip = client_ip(peer_ip, request.headers(), state.config.server.trust_proxy);
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let path = request.uri().path().to_string();
    let query = request.uri().query().map(str::to_string);
    let method = request.method().clone();

    state.metrics.requests_total.fetch_add(1, Ordering::Relaxed);

    if !is_safe_path(&path) {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_path",
            "request path is not a valid absolute path",
        );
    }

    // Reject obviously oversized bodies before touching the tunnel.
    let max_body = state.config.limits.max_request_bytes.bytes();
    if let Some(value) = request.headers().get(header::CONTENT_LENGTH) {
        if let Ok(len) = value.to_str().unwrap_or("0").trim().parse::<u64>() {
            if len > max_body as u64 {
                return error_response(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "request_too_large",
                    &format!("request body exceeds the {max_body} byte limit"),
                );
            }
        }
    }
    let (mut parts, body) = request.into_parts();
    let limited = http_body_util::Limited::new(body, max_body);
    let body = Body::new(limited);
    parts.extensions.insert(PeerAddr(ip));
    let request = Request::from_parts(parts, body);

    let Some(resolved) = state.registry.resolve(host.as_deref(), &path) else {
        state
            .metrics
            .tunnel_unavailable
            .fetch_add(1, Ordering::Relaxed);
        return error_response(
            StatusCode::NOT_FOUND,
            "no_route",
            "no tunnel or static route matches this path/host",
        );
    };

    match resolved {
        Resolved::Tunnel { tunnel, path } => {
            forward_tunnel(
                state, tunnel, path, query, method, request, ip, request_id, started,
            )
            .await
        }
        Resolved::Static { route, path } => {
            forward_static(
                state, route, path, query, method, request, ip, request_id, started,
            )
            .await
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn forward_static(
    state: Arc<ServerState>,
    route: Arc<StaticRoute>,
    path: String,
    query: Option<String>,
    method: axum::http::Method,
    request: Request<Body>,
    ip: IpAddr,
    request_id: String,
    started: std::time::Instant,
) -> HttpResponse<Body> {
    if route.settings.require_auth {
        if let Err(err) = authenticate_request(&state, request.headers(), ip) {
            state.metrics.auth_failures.fetch_add(1, Ordering::Relaxed);
            let mut response = error_response(err.status(), err.code(), &err.to_string());
            if let Some(retry) = err.retry_after() {
                set_retry_after(&mut response, retry);
            }
            return response;
        }
    }
    let path = if route.settings.strip_prefix {
        path
    } else {
        request.uri().path().to_string()
    };
    let target = match route.target_uri(&path, query.as_deref()) {
        Ok(target) => target,
        Err(err) => {
            warn!(request_id = %request_id, error = %err, "invalid static upstream URI");
            return error_response(
                StatusCode::BAD_GATEWAY,
                "bad_upstream_uri",
                "invalid upstream URI",
            );
        }
    };
    let (parts, body) = request.into_parts();
    let ctx = ForwardCtx {
        tunnel_id: "static",
        client_ip: ip,
        request_id: &request_id,
        original_host: parts
            .headers
            .get(header::HOST)
            .and_then(|v| v.to_str().ok()),
        scheme: "https",
        upstream_key: None,
        upstream_auth: UpstreamAuth::Passthrough,
    };
    let mut headers = sanitize_request_headers(&parts.headers, &ctx);
    for (name, value) in &route.settings.headers {
        if let (Ok(name), Ok(value)) = (
            header::HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            headers.insert(name, value);
        }
    }
    let mut builder = Request::builder().method(method).uri(target);
    if let Some(h) = builder.headers_mut() {
        *h = headers;
    }
    let Ok(upstream_request) = builder.body(body) else {
        return error_response(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "cannot build upstream request",
        );
    };
    match crate::upstream::send_request_with_timeout(
        &route.client,
        upstream_request,
        state.config.limits.request_timeout,
    )
    .await
    {
        Ok(response) => {
            let (parts, incoming) = response.into_parts();
            let elapsed = started.elapsed();
            debug!(
                request_id = %request_id,
                mount = %route.mount,
                status = %parts.status,
                ms = elapsed.as_millis() as u64,
                "static route forwarded"
            );
            build_response_guarded(
                parts.status,
                parts.headers,
                incoming,
                state.config.limits.stream_idle_timeout,
                state.registry.metrics().tunnel("static"),
                Some(request_id),
                BodyGuards {
                    permit: None,
                    conn_task: None,
                    session: None,
                },
            )
        }
        Err(err) => {
            warn!(request_id = %request_id, error = %err, "static upstream failed");
            let mut response = match err {
                Error::Timeout(_) => error_response(
                    StatusCode::GATEWAY_TIMEOUT,
                    "upstream_timeout",
                    "static upstream did not respond in time",
                ),
                _ => error_response(
                    StatusCode::BAD_GATEWAY,
                    "upstream_unreachable",
                    "cannot reach the static upstream",
                ),
            };
            response.headers_mut().insert(
                "x-divertlink-request-id",
                HeaderValue::from_str(&request_id).unwrap_or(HeaderValue::from_static("dl")),
            );
            response
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn forward_tunnel(
    state: Arc<ServerState>,
    tunnel: Arc<TunnelEntry>,
    path: String,
    query: Option<String>,
    method: axum::http::Method,
    request: Request<Body>,
    ip: IpAddr,
    request_id: String,
    started: std::time::Instant,
) -> HttpResponse<Body> {
    let public = matches_public_path(&path, &tunnel.settings.public_paths);
    let stats = tunnel.stats.clone();

    // Authentication (skipped for explicitly public paths such as /health).
    let (entry, guard) = if public {
        (None, None)
    } else {
        match authenticate(&state, request.headers(), ip, &tunnel) {
            Ok((entry, guard)) => (Some(entry), Some(guard)),
            Err(err) => {
                state.metrics.auth_failures.fetch_add(1, Ordering::Relaxed);
                if matches!(
                    err,
                    AuthError::RateLimited(_) | AuthError::Busy | AuthError::TunnelDenied
                ) {
                    state.metrics.rate_limited.fetch_add(1, Ordering::Relaxed);
                }
                let mut response = error_response(err.status(), err.code(), &err.to_string());
                if let Some(retry) = err.retry_after() {
                    set_retry_after(&mut response, retry);
                }
                stats.inc_error();
                return response;
            }
        }
    };
    let key_name = entry.map(|e| e.name.clone());

    // Open the data stream towards the agent (per-tunnel concurrency limit).
    let (stream, permit) = match tunnel.open_stream().await {
        Ok(pair) => pair,
        Err(err) => {
            state
                .metrics
                .tunnel_unavailable
                .fetch_add(1, Ordering::Relaxed);
            stats.inc_error();
            let response = match &err {
                Error::Limit(_) => {
                    let mut response = error_response(
                        StatusCode::TOO_MANY_REQUESTS,
                        "tunnel_busy",
                        &err.to_string(),
                    );
                    set_retry_after(&mut response, Duration::from_millis(250));
                    response
                }
                _ => error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "tunnel_unavailable",
                    &err.to_string(),
                ),
            };
            return response;
        }
    };
    drop(guard); // concurrency slot is released once the stream is established

    // If the tunnel lost its session between resolution and stream open, the
    // error above would have been unavailable; here the session is live.
    let session: Arc<ActiveSession> = match tunnel.session() {
        Some(session) => session,
        None => {
            stats.inc_error();
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "tunnel_unavailable",
                "tunnel disconnected",
            );
        }
    };

    let (parts, body) = request.into_parts();
    let ctx = ForwardCtx {
        tunnel_id: &tunnel.settings.id,
        client_ip: ip,
        request_id: &request_id,
        original_host: parts
            .headers
            .get(header::HOST)
            .and_then(|v| v.to_str().ok()),
        scheme: "https",
        upstream_key: tunnel.settings.upstream_key(),
        upstream_auth: tunnel.settings.upstream_auth_header,
    };
    let headers = sanitize_request_headers(&parts.headers, &ctx);
    let uri_string = match &query {
        Some(q) if !q.is_empty() => format!("{path}?{q}"),
        _ => path,
    };
    let mut builder = Request::builder().method(method).uri(&uri_string);
    if let Some(h) = builder.headers_mut() {
        *h = headers;
        // The data stream carries exactly one request.
        h.insert(header::CONNECTION, HeaderValue::from_static("close"));
    }
    let upstream_request = match builder.body(body) {
        Ok(request) => request,
        Err(err) => {
            stats.inc_error();
            return error_response(StatusCode::BAD_REQUEST, "bad_request", &format!("{err}"));
        }
    };

    stats.inc_request();
    stats.inc_active();

    use tokio_util::compat::FuturesAsyncReadCompatExt;
    let io = hyper_util::rt::TokioIo::new(stream.compat());
    let (mut sender, conn) = match hyper::client::conn::http1::Builder::new()
        .handshake::<_, Body>(io)
        .await
    {
        Ok(pair) => pair,
        Err(err) => {
            stats.dec_active();
            stats.inc_error();
            state.metrics.stream_errors.fetch_add(1, Ordering::Relaxed);
            return error_response(
                StatusCode::BAD_GATEWAY,
                "tunnel_stream_error",
                &format!("cannot establish tunnel stream: {err}"),
            );
        }
    };
    let conn_task = tokio::spawn(async move {
        if let Err(err) = conn.await {
            debug!(error = %err, "tunnel data stream connection ended");
        }
    });

    let response = tokio::time::timeout(
        tunnel.request_timeout,
        sender.send_request(upstream_request),
    )
    .await;
    let response = match response {
        Ok(Ok(response)) => response,
        Ok(Err(err)) => {
            conn_task.abort();
            stats.dec_active();
            stats.inc_error();
            state.metrics.stream_errors.fetch_add(1, Ordering::Relaxed);
            return error_response(
                StatusCode::BAD_GATEWAY,
                "tunnel_request_failed",
                &format!("tunnel request failed: {err}"),
            );
        }
        Err(_) => {
            conn_task.abort();
            stats.dec_active();
            stats.inc_error();
            return error_response(
                StatusCode::GATEWAY_TIMEOUT,
                "upstream_timeout",
                "model server did not respond in time",
            );
        }
    };

    let (parts, incoming) = response.into_parts();
    let elapsed = started.elapsed().as_millis() as u64;
    debug!(
        request_id = %request_id,
        tunnel = %tunnel.settings.id,
        status = %parts.status,
        ms = elapsed,
        key = key_name.as_deref().unwrap_or("public"),
        "tunnel request forwarded"
    );
    stats.touch();
    if parts.status.as_u16() >= 500 {
        stats.inc_error();
    }
    let guards = BodyGuards {
        permit: Some(permit),
        conn_task: Some(conn_task),
        session: Some(session),
    };
    build_response_guarded(
        parts.status,
        parts.headers,
        incoming,
        tunnel.stream_idle_timeout,
        stats,
        Some(request_id),
        guards,
    )
}

/// Lifetimes kept alive until the response body is fully consumed or dropped.
#[derive(Debug)]
#[allow(dead_code)] // fields exist to keep permits/tasks alive, not to be read
struct BodyGuards {
    permit: Option<OwnedSemaphorePermit>,
    conn_task: Option<tokio::task::JoinHandle<()>>,
    session: Option<Arc<ActiveSession>>,
}

impl Drop for BodyGuards {
    fn drop(&mut self) {
        if let Some(task) = self.conn_task.take() {
            task.abort();
        }
        // `permit` and `session` drop naturally here.
    }
}

/// Authenticate an ingress request. When no API keys are configured the gateway
/// runs without ingress authentication (the startup log warns loudly).
fn authenticate_request(
    state: &ServerState,
    headers: &axum::http::HeaderMap,
    ip: IpAddr,
) -> Result<Option<KeyGuard>, AuthError> {
    if !state.keys.is_enabled() {
        return Ok(None);
    }
    let entry = state.keys.authenticate(headers, ip)?;
    entry.check_rate()?;
    entry.acquire().map(Some)
}

fn authenticate(
    state: &ServerState,
    headers: &axum::http::HeaderMap,
    ip: IpAddr,
    tunnel: &TunnelEntry,
) -> Result<(Arc<crate::auth::KeyEntry>, Option<KeyGuard>), AuthError> {
    if !state.keys.is_enabled() {
        return Ok((Arc::new(crate::auth::KeyEntry::anonymous()), None));
    }
    let entry = state.keys.authenticate(headers, ip)?;
    entry.authorize_tunnel(&tunnel.settings.id)?;
    entry.check_rate()?;
    let guard = entry.acquire()?;
    Ok((entry, Some(guard)))
}

/// Convert an upstream response into a streaming public response.
#[allow(clippy::too_many_arguments)]
fn build_response_guarded(
    status: StatusCode,
    headers: axum::http::HeaderMap,
    incoming: Incoming,
    idle_timeout: Duration,
    stats: Arc<TunnelStats>,
    request_id: Option<String>,
    guards: BodyGuards,
) -> HttpResponse<Body> {
    let headers = sanitize_response_headers(&headers);
    let mut response = HttpResponse::new(stream_body(incoming, idle_timeout, stats, guards));
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    if let Some(id) = request_id {
        if let Ok(value) = HeaderValue::from_str(&id) {
            response
                .headers_mut()
                .insert("x-divertlink-request-id", value);
        }
    }
    if response.headers().contains_key(header::CONTENT_LENGTH) {
        // Length is preserved from upstream; the body is byte identical.
    }
    response
}

struct StreamState {
    incoming: Option<Incoming>,
    idle: Duration,
    stats: Arc<TunnelStats>,
    finished: bool,
    /// Dropped with the body: releases the stream permit and aborts the conn task.
    guards: Option<BodyGuards>,
}

/// Streaming response body with an idle timeout and byte accounting.
fn stream_body(
    incoming: Incoming,
    idle: Duration,
    stats: Arc<TunnelStats>,
    guards: BodyGuards,
) -> Body {
    let state = StreamState {
        incoming: Some(incoming),
        idle,
        stats,
        finished: false,
        guards: Some(guards),
    };
    let stream = futures_util::stream::unfold(state, |mut state| async move {
        if state.finished {
            return None;
        }
        let mut incoming = state.incoming.take()?;
        match tokio::time::timeout(state.idle, incoming.frame()).await {
            Err(_) => {
                warn!("upstream response body idle timeout");
                state.finished = true;
                state.guards = None;
                // `state` is dropped when the stream ends; the guards release here.
                let _ = state.incoming.take();
                Some((
                    Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "upstream stream idle timeout",
                    )),
                    state,
                ))
            }
            Ok(None) => {
                state.finished = true;
                state.guards = None;
                let _ = state.incoming.take();
                None
            }
            Ok(Some(Ok(frame))) => {
                state.incoming = Some(incoming);
                match frame.into_data() {
                    Ok(data) => {
                        state.stats.add_bytes_out(data.len() as u64);
                        Some((Ok(data), state))
                    }
                    Err(_trailers) => Some((Ok(Bytes::new()), state)),
                }
            }
            Ok(Some(Err(err))) => {
                state.finished = true;
                Some((
                    Err(std::io::Error::other(format!(
                        "upstream stream error: {err}"
                    ))),
                    state,
                ))
            }
        }
    });
    Body::from_stream(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::TunnelStats;

    #[test]
    fn stream_body_type_checks() {
        let _ = std::any::type_name::<fn(Incoming, Duration, Arc<TunnelStats>) -> Body>();
    }
}
