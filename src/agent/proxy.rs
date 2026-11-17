//! Agent data-stream proxy: HTTP/1.1 over a yamux stream to the local model server.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::header::{self, HeaderMap, HeaderValue};
use axum::http::{Request, Response, StatusCode};
use bytes::Bytes;
use futures_util::stream::Stream;
use http_body::Frame;
use hyper_util::rt::TokioIo;
use tokio_util::compat::FuturesAsyncReadCompatExt;
use tracing::{debug, warn};

use super::AgentState;
use crate::error::Error;
use crate::httpfwd::{
    error_response, is_hop_by_hop, join_upstream_uri, new_request_id, sanitize_response_headers,
    IdleTimeoutBody,
};
use crate::metrics::TunnelStats;

/// Idle timeout applied while streaming a local model response.
pub const TARGET_STREAM_IDLE: Duration = Duration::from_secs(300);

/// Maximum time the gateway has to send complete request headers for a data stream.
pub const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Serve one data stream as an HTTP/1.1 server.
pub async fn serve_stream(stream: yamux::Stream, state: Arc<AgentState>) {
    let io = TokioIo::new(stream.compat());
    let service = hyper::service::service_fn(move |request| {
        let state = state.clone();
        async move { handle_request(request, state).await }
    });
    let mut builder = hyper::server::conn::http1::Builder::new();
    // Every tunnel stream carries exactly one request.
    builder
        .keep_alive(false)
        .half_close(false)
        .timer(hyper_util::rt::TokioTimer::new())
        .header_read_timeout(Some(HEADER_READ_TIMEOUT));
    if let Err(err) = builder.serve_connection(io, service).await {
        debug!(error = %err, "data stream ended with error");
    }
}

async fn handle_request(
    request: Request<hyper::body::Incoming>,
    state: Arc<AgentState>,
) -> std::result::Result<Response<Body>, std::convert::Infallible> {
    let request_id = new_request_id();
    let started = std::time::Instant::now();
    let request_path = request.uri().path().to_string();
    let query = request.uri().query().map(str::to_string);
    let method = request.method().clone();
    // Pick the upstream by longest matching prefix; the prefix is stripped.
    let (route, path) = state.route_for(&request_path);
    let target = match join_upstream_uri(&route.uri, &path, query.as_deref()) {
        Ok(target) => target,
        Err(err) => {
            warn!(request_id = %request_id, error = %err, "invalid upstream URI");
            return Ok(error_response(
                StatusCode::BAD_GATEWAY,
                "bad_target_uri",
                "cannot build the local target URI",
            ));
        }
    };

    let (parts, body) = request.into_parts();
    let mut headers = HeaderMap::with_capacity(parts.headers.len() + 4);
    for (name, value) in parts.headers.iter() {
        if is_hop_by_hop(name) || name == header::HOST {
            continue;
        }
        headers.append(name.clone(), value.clone());
    }
    for (name, value) in &route.headers {
        if let (Ok(name), Ok(value)) = (
            header::HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            headers.insert(name, value);
        }
    }
    if let Some(authority) = target.authority() {
        if let Ok(value) = HeaderValue::from_str(authority.as_str()) {
            headers.insert(header::HOST, value);
        }
    }
    let mut builder = Request::builder().method(method).uri(target);
    if let Some(h) = builder.headers_mut() {
        *h = headers;
    }
    let upstream_request = match builder.body(Body::new(body)) {
        Ok(request) => request,
        Err(err) => {
            return Ok(error_response(
                StatusCode::BAD_REQUEST,
                "bad_request",
                &format!("cannot build local request: {err}"),
            ))
        }
    };

    state.metrics.requests_total.fetch_add(1, Ordering::Relaxed);
    state.metrics.connections_active.load(Ordering::Relaxed);
    let stats = state.metrics.tunnel(&state.config.agent.id);
    stats.inc_request();
    stats.inc_active();

    let response = tokio::time::timeout(
        route.request_timeout,
        route.client.request(upstream_request),
    )
    .await;
    let response = match response {
        Ok(Ok(response)) => response,
        Ok(Err(err)) => {
            stats.dec_active();
            stats.inc_error();
            warn!(request_id = %request_id, error = %err, "local target request failed");
            return Ok(error_response(
                StatusCode::BAD_GATEWAY,
                "target_unreachable",
                &format!("local model server unreachable: {err}"),
            ));
        }
        Err(_) => {
            stats.dec_active();
            stats.inc_error();
            return Ok(error_response(
                StatusCode::GATEWAY_TIMEOUT,
                "target_timeout",
                "local model server did not answer in time",
            ));
        }
    };

    let (parts, incoming) = response.into_parts();
    let mut response_headers = sanitize_response_headers(&parts.headers);
    crate::httpfwd::apply_stream_headers(&mut response_headers);
    debug!(
        request_id = %request_id,
        path = %request_path,
        target = %route.name,
        status = %parts.status,
        ms = started.elapsed().as_millis() as u64,
        "local target responded"
    );
    stats.touch();
    if parts.status.as_u16() >= 500 {
        stats.inc_error();
    }
    let mut response = Response::new(stream_body(incoming, stats));
    *response.status_mut() = parts.status;
    *response.headers_mut() = response_headers;
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response
            .headers_mut()
            .insert("x-divertlink-request-id", value);
    }
    if let Ok(value) = HeaderValue::from_str(&route.name) {
        response.headers_mut().insert("x-divertlink-target", value);
    }
    Ok(response)
}

fn stream_body(incoming: hyper::body::Incoming, stats: Arc<TunnelStats>) -> Body {
    let body = IdleTimeoutBody::new(incoming, TARGET_STREAM_IDLE);
    let stream = futures_util::stream::unfold(
        (body, stats, false),
        |(mut body, stats, finished)| async move {
            if finished {
                return None;
            }
            use http_body::Body as _;
            match std::future::poll_fn(|cx| std::pin::Pin::new(&mut body).poll_frame(cx)).await {
                Some(Ok(frame)) => match frame.into_data() {
                    Ok(data) => {
                        stats.add_bytes_in(data.len() as u64);
                        Some((Ok::<_, std::io::Error>(data), (body, stats, false)))
                    }
                    Err(_trailers) => Some((Ok(Bytes::new()), (body, stats, false))),
                },
                Some(Err(err)) => Some((
                    Err(std::io::Error::other(err.to_string())),
                    (body, stats, true),
                )),
                None => None,
            }
        },
    );
    Body::from_stream(stream)
}

/// Re-exported helper for tests.
pub fn frame_type_marker(_f: Frame<Bytes>) -> impl Stream<Item = Bytes> {
    futures_util::stream::empty()
}

/// Error mapping helper shared with tests.
pub fn map_error(err: &Error) -> StatusCode {
    match err {
        Error::Timeout(_) => StatusCode::GATEWAY_TIMEOUT,
        _ => StatusCode::BAD_GATEWAY,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_errors() {
        assert_eq!(
            map_error(&Error::Timeout("x".into())),
            StatusCode::GATEWAY_TIMEOUT
        );
        assert_eq!(
            map_error(&Error::Internal("x".into())),
            StatusCode::BAD_GATEWAY
        );
    }

    #[test]
    fn idle_timeout_is_reasonable() {
        assert!(TARGET_STREAM_IDLE.as_secs() >= 120);
    }
}
