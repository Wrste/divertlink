//! End-to-end tests: real gateway + real agent + mock model server over TLS.
//!
//! These tests exercise the whole path: TLS handshake with ALPN demultiplexing,
//! yamux multiplexing, agent handshake, streaming SSE passthrough, API key auth,
//! rate limiting, static routes, cancellation propagation and reconnect.

use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bytes::Bytes;
use http_body_util::BodyExt;
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use divertlink::agent::{self, AgentHandle};
use divertlink::config::{
    AgentConfig, ApiKeySettings, HeartbeatMode, ServerConfig, StaticRouteSettings, TunnelSettings,
    UpstreamAuth,
};
use divertlink::server::{self, ServerHandle};
use divertlink::tls::{self, GenCertsOptions};
use divertlink::upstream;

// ---------------------------------------------------------------------------
// Mock model server
// ---------------------------------------------------------------------------

#[derive(Default)]
struct UpstreamState {
    inflight: AtomicI64,
    cancelled: AtomicU64,
    last_auth: Mutex<Vec<String>>,
}

impl UpstreamState {
    async fn auth_seen(&self) -> Vec<String> {
        self.last_auth.lock().await.clone()
    }
}

/// Decrements the inflight counter when the response stream is dropped.
struct InflightGuard(Arc<UpstreamState>);

impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.0.inflight.fetch_sub(1, Ordering::SeqCst);
        self.0.cancelled.fetch_add(1, Ordering::SeqCst);
    }
}

async fn upstream_app(state: Arc<UpstreamState>) -> Router {
    Router::new()
        .route("/v1/models", get(mock_models))
        .route("/v1/chat/completions", post(mock_chat))
        .route("/v1/messages", post(mock_messages))
        .route("/echo", post(mock_echo))
        .route("/sse", get(mock_sse))
        .route("/slow", get(mock_slow))
        .route("/health", get(mock_health))
        .layer(axum::extract::DefaultBodyLimit::disable())
        .with_state(state)
}

async fn mock_models(State(state): State<Arc<UpstreamState>>) -> Json<Value> {
    let mut auth = state.last_auth.lock().await;
    auth.clear();
    Json(json!({"object": "list", "data": [{"id": "mock-model", "object": "model"}]}))
}

async fn mock_chat(State(state): State<Arc<UpstreamState>>, body: Bytes) -> Response {
    let value: Value = serde_json::from_slice(&body).unwrap_or_else(|_| json!({}));
    let stream = value
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    state.inflight.fetch_add(1, Ordering::SeqCst);
    let guard = InflightGuard(state.clone());
    if stream {
        let chunks = 8usize;
        let stream = futures_util::stream::unfold(
            (0usize, Some(guard)),
            move |(i, guard)| async move {
                if i >= chunks {
                    return None;
                }
                tokio::time::sleep(Duration::from_millis(40)).await;
                let payload = format!(
                    "data: {{\"id\":\"mock\",\"choices\":[{{\"delta\":{{\"content\":\"tok{i}\"}}}}]}}\n\n"
                );
                Some((
                    Ok::<_, std::io::Error>(Bytes::from(payload)),
                    (i + 1, guard),
                ))
            },
        );
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .header(header::CACHE_CONTROL, "no-cache")
            .body(Body::from_stream(stream))
            .unwrap()
    } else {
        Json(json!({
            "id": "chatcmpl-mock",
            "object": "chat.completion",
            "model": "mock-model",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "mock-response"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5}
        }))
        .into_response()
    }
}

async fn mock_messages(
    State(state): State<Arc<UpstreamState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    state.inflight.fetch_add(1, Ordering::SeqCst);
    let _guard = InflightGuard(state.clone());
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let api_key = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    {
        let mut seen = state.last_auth.lock().await;
        seen.clear();
        seen.push(format!("authorization:{auth}"));
        seen.push(format!("x-api-key:{api_key}"));
    }
    let _ = body;
    Json(json!({
        "id": "msg_mock",
        "type": "message",
        "role": "assistant",
        "model": "claude-mock",
        "content": [{"type": "text", "text": "anthropic-response"}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 3, "output_tokens": 2}
    }))
    .into_response()
}

async fn mock_echo(body: Bytes) -> Response {
    let mut hasher = Sha256::new();
    hasher.update(&body);
    let digest = hex(&hasher.finalize());
    let len = body.len();
    let mut response = Response::new(Body::from(body));
    response.headers_mut().insert(
        "x-echo-length",
        HeaderValue::from_str(&len.to_string()).unwrap(),
    );
    response
        .headers_mut()
        .insert("x-echo-sha256", HeaderValue::from_str(&digest).unwrap());
    response
}

async fn mock_sse(State(state): State<Arc<UpstreamState>>) -> Response {
    state.inflight.fetch_add(1, Ordering::SeqCst);
    let guard = InflightGuard(state.clone());
    let stream =
        futures_util::stream::unfold((0usize, Some(guard)), move |(i, guard)| async move {
            if i >= 10 {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(80)).await;
            Some((
                Ok::<_, std::io::Error>(Bytes::from(format!("data: chunk-{i}\n\n"))),
                (i + 1, guard),
            ))
        });
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from_stream(stream))
        .unwrap()
}

async fn mock_slow(State(state): State<Arc<UpstreamState>>) -> Response {
    state.inflight.fetch_add(1, Ordering::SeqCst);
    let _guard = InflightGuard(state.clone());
    tokio::time::sleep(Duration::from_secs(30)).await;
    Json(json!({"slow": true})).into_response()
}

async fn mock_health() -> &'static str {
    "ok"
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

struct MockUpstream {
    addr: SocketAddr,
    state: Arc<UpstreamState>,
    shutdown: CancellationToken,
}

async fn start_mock_upstream() -> MockUpstream {
    let state = Arc::new(UpstreamState::default());
    let app = upstream_app(state.clone()).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let shutdown = CancellationToken::new();
    let token = shutdown.clone();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move { token.cancelled().await })
            .await
            .ok();
    });
    MockUpstream {
        addr,
        state,
        shutdown,
    }
}

// ---------------------------------------------------------------------------
// Test rig
// ---------------------------------------------------------------------------

fn init_tracing() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_test_writer()
            .try_init();
    });
}

fn reserve_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

struct Rig {
    _tmp: TempDir,
    ingress_port: u16,
    admin: SocketAddr,
    gateway: Option<ServerHandle>,
    agent: Option<AgentHandle>,
    client: Client<HttpsConnector<HttpConnector>, Body>,
    ca: PathBuf,
    server_config: ServerConfig,
    dead: bool,
}

struct RigBuilder {
    server_config: ServerConfig,
    agent_config: AgentConfig,
}

impl RigBuilder {
    fn new(upstream: SocketAddr, ca: PathBuf, port: u16) -> Self {
        let mut server_config = ServerConfig::default();
        server_config.server.name = "test-gateway".into();
        server_config.server.tunnel_listen = Some(SocketAddr::from(([127, 0, 0, 1], port)));
        server_config.server.ingress_listen = Some(SocketAddr::from(([127, 0, 0, 1], port)));
        server_config.server.http_listen = None;
        server_config.server.admin_listen = Some(SocketAddr::from(([127, 0, 0, 1], 0)));
        server_config.tls.cert = PathBuf::from("certs/server.crt");
        server_config.tls.key = PathBuf::from("certs/server.key");
        server_config.limits.max_request_bytes = divertlink::util::ByteSize(16 * 1024 * 1024);
        server_config.limits.request_timeout = Duration::from_secs(10);
        server_config.limits.stream_idle_timeout = Duration::from_secs(5);
        server_config.api_keys = vec![ApiKeySettings {
            name: "newapi".into(),
            key: "dlk_test_key".into(),
            allow_tunnels: vec![],
            allow_ips: vec![],
            rate_limit: None,
            max_concurrent: 0,
        }];
        server_config.tunnels = vec![TunnelSettings {
            id: "home".into(),
            token: "agt_test_token".into(),
            mount: Some("/home".into()),
            hosts: vec![],
            default: false,
            protocols: vec!["openai".into(), "anthropic".into(), "vllm".into()],
            upstream_api_key: None,
            upstream_auth_header: UpstreamAuth::Passthrough,
            public_paths: vec!["/health".into()],
            max_streams: None,
            request_timeout: None,
            enabled: true,
        }];

        let mut agent_config = AgentConfig::default();
        agent_config.agent.id = "home".into();
        agent_config.agent.server = format!("127.0.0.1:{port}");
        agent_config.agent.token = "agt_test_token".into();
        agent_config.agent.heartbeat = Duration::from_millis(300);
        agent_config.agent.heartbeat_timeout = Duration::from_millis(1500);
        agent_config.agent.heartbeat_mode = HeartbeatMode::Both;
        agent_config.agent.reconnect_min = Duration::from_millis(100);
        agent_config.agent.reconnect_max = Duration::from_millis(500);
        agent_config.agent.reconnect_jitter = 0.1;
        agent_config.tls.ca = Some(ca);
        agent_config.target.url = format!("http://{upstream}");

        RigBuilder {
            server_config,
            agent_config,
        }
    }

    fn build(self) -> Self {
        self
    }
}

impl Rig {
    async fn start_with(builder: RigBuilder, dir: PathBuf) -> Rig {
        init_tracing();
        let server_config = builder.server_config;
        let mut agent_config = builder.agent_config;
        let _ = &dir;
        let ca = agent_config.tls.ca.clone().expect("test CA");
        let client = upstream::build_http_client(Some(&ca), Duration::from_secs(5)).unwrap();
        let tmp = TempDir::new().unwrap();
        // Keep the temp dir alive with the rig by leaking ownership into `_tmp`.
        let tmp = {
            std::mem::forget(tmp);
            TempDir::new().unwrap()
        };
        let gateway = server::run(server_config.clone(), None)
            .await
            .expect("gateway starts");
        let addrs = gateway.addrs();
        let ingress_port = addrs
            .ingress
            .or(addrs.tunnel)
            .expect("ingress listener")
            .port();
        let admin = addrs.admin.expect("admin listener");
        agent_config.agent.server = format!("127.0.0.1:{ingress_port}");
        let agent = agent::run(agent_config.clone(), None)
            .await
            .expect("agent starts");
        let rig = Rig {
            _tmp: tmp,
            ingress_port,
            admin,
            gateway: Some(gateway),
            agent: Some(agent),
            client,
            ca,
            server_config,
            dead: false,
        };
        rig.wait_ready(Duration::from_secs(10)).await;
        rig
    }

    fn cert_path(&self) -> &Path {
        &self.ca
    }

    fn https_url(&self, path: &str) -> String {
        format!("https://127.0.0.1:{}{}", self.ingress_port, path)
    }

    async fn wait_ready(&self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Ok((status, _, body)) = self.admin_get("/readyz").await {
                if status == StatusCode::OK {
                    let _ = body;
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("gateway never became ready");
    }

    async fn wait_not_ready(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Ok((status, _, _)) = self.admin_get("/readyz").await {
                if status != StatusCode::OK {
                    return true;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        false
    }

    async fn admin_get(&self, path: &str) -> Result<(StatusCode, HeaderMap, Bytes), String> {
        let url = format!("http://{}{}", self.admin, path);
        let request = Request::builder()
            .uri(url)
            .body(Body::empty())
            .map_err(|e| e.to_string())?;
        let response = self
            .client
            .request(request)
            .await
            .map_err(|e| e.to_string())?;
        let (parts, body) = response.into_parts();
        let bytes = body.collect().await.map_err(|e| e.to_string())?.to_bytes();
        Ok((parts.status, parts.headers, bytes))
    }

    async fn request(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Body,
    ) -> (StatusCode, HeaderMap, Bytes) {
        let mut builder = Request::builder().method(method).uri(self.https_url(path));
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let request = builder
            .body(body)
            .unwrap_or_else(|_| Request::new(Body::empty()));
        let response = self.client.request(request).await.expect("ingress request");
        let (parts, body) = response.into_parts();
        let bytes = body.collect().await.expect("response body").to_bytes();
        (parts.status, parts.headers, bytes)
    }

    async fn get(&self, path: &str, key: Option<&str>) -> (StatusCode, HeaderMap, Bytes) {
        let mut headers: Vec<(&str, &str)> = Vec::new();
        if let Some(key) = key {
            headers.push(("authorization", key));
        }
        self.request("GET", path, &headers, Body::empty()).await
    }

    async fn post_json(
        &self,
        path: &str,
        key: Option<&str>,
        value: Value,
    ) -> (StatusCode, HeaderMap, Bytes) {
        let mut headers: Vec<(&str, &str)> = vec![("content-type", "application/json")];
        if let Some(key) = key {
            headers.push(("authorization", key));
        }
        self.request(
            "POST",
            path,
            &headers,
            Body::from(serde_json::to_vec(&value).unwrap()),
        )
        .await
    }

    /// Restart the gateway on the same port, keeping the agent running.
    async fn restart_gateway(&mut self) {
        if let Some(gateway) = self.gateway.take() {
            gateway.shutdown().await;
        }
        let gateway = server::run(self.server_config.clone(), None)
            .await
            .expect("gateway restarts");
        let addrs = gateway.addrs();
        self.admin = addrs.admin.expect("admin listener after restart");
        self.gateway = Some(gateway);
        self.wait_ready(Duration::from_secs(15)).await;
    }

    async fn shutdown(mut self) {
        if self.dead {
            return;
        }
        self.dead = true;
        if let Some(agent) = self.agent.take() {
            agent.shutdown().await;
        }
        if let Some(gateway) = self.gateway.take() {
            gateway.shutdown().await;
        }
    }
}

async fn new_rig(mut customize: impl FnMut(&mut RigBuilder)) -> (Rig, MockUpstream) {
    let upstream = start_mock_upstream().await;
    let tmp = TempDir::new().unwrap();
    let certs = tls::generate_certs(&GenCertsOptions {
        dir: tmp.path().join("certs"),
        domain: "localhost".into(),
        client_cert: false,
        self_signed: false,
        force: false,
    })
    .unwrap();
    let port = reserve_port();
    let mut builder = RigBuilder::new(upstream.addr, certs.ca_cert.clone(), port);
    builder.server_config.tls.cert = certs.server_cert.clone();
    builder.server_config.tls.key = certs.server_key.clone();
    customize(&mut builder);
    // Rig takes ownership of temp dirs itself; re-generate nothing here.
    let builder = builder.build();
    let rig = Rig::start_with(builder, tmp.path().to_path_buf()).await;
    // Keep the certificate temp dir alive for the duration of the test.
    std::mem::forget(tmp);
    (rig, upstream)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn openai_json_roundtrip() {
    let (rig, upstream) = new_rig(|_| {}).await;
    let (status, headers, body) = rig
        .post_json(
            "/home/v1/chat/completions",
            Some("Bearer dlk_test_key"),
            json!({"model": "mock-model", "messages": [{"role": "user", "content": "hi"}]}),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "body: {}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(
        headers.get(header::CONTENT_TYPE).unwrap(),
        "application/json"
    );
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["choices"][0]["message"]["content"], "mock-response");
    assert!(headers.contains_key("x-divertlink-request-id"));
    let _ = upstream;
    rig.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sse_streams_incrementally() {
    let (rig, _upstream) = new_rig(|_| {}).await;
    let request = Request::builder()
        .method("POST")
        .uri(rig.https_url("/home/v1/chat/completions"))
        .header("authorization", "Bearer dlk_test_key")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&json!({"model": "mock-model", "stream": true})).unwrap(),
        ))
        .unwrap();
    let started = Instant::now();
    let response = rig.client.request(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE).unwrap(),
        "text/event-stream"
    );
    let mut body = response.into_body();
    let mut first_frame_at = None;
    let mut frames = 0usize;
    let mut text = String::new();
    while let Some(frame) = body.frame().await {
        let frame = frame.unwrap();
        if let Some(data) = frame.data_ref() {
            frames += 1;
            if first_frame_at.is_none() {
                first_frame_at = Some(started.elapsed());
            }
            text.push_str(&String::from_utf8_lossy(data));
        }
    }
    let total = started.elapsed();
    assert!(frames >= 8, "expected at least 8 SSE frames, got {frames}");
    assert!(text.contains("tok0") && text.contains("tok7"), "{text}");
    let first = first_frame_at.expect("first frame");
    // The mock writes a chunk every 40ms; buffering would delay the first frame
    // to >= ~320ms (all chunks) instead of ~40-100ms.
    assert!(
        first < Duration::from_millis(300),
        "first SSE frame took {first:?}: the tunnel is buffering"
    );
    assert!(
        total >= Duration::from_millis(200),
        "stream finished too fast to be incremental: {total:?}"
    );
    rig.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn anthropic_auth_passthrough() {
    let (rig, upstream) = new_rig(|_| {}).await;
    let (status, _, body) = rig
        .request(
            "POST",
            "/home/v1/messages",
            &[
                ("x-api-key", "sk-ant-client"),
                ("anthropic-version", "2023-06-01"),
                ("content-type", "application/json"),
            ],
            Body::from(
                serde_json::to_vec(
                    &json!({"model": "claude-mock", "max_tokens": 16, "messages": []}),
                )
                .unwrap(),
            ),
        )
        .await;
    // Gateway authentication also accepts x-api-key; "sk-ant-client" is not a
    // configured gateway key, so this must be rejected before reaching the agent.
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "{}",
        String::from_utf8_lossy(&body)
    );

    // Now with the gateway key as x-api-key; the client's anthropic key must be
    // forwarded untouched (passthrough mode).
    let (status, _, body) = rig
        .request(
            "POST",
            "/home/v1/messages",
            &[
                ("x-api-key", "dlk_test_key"),
                ("content-type", "application/json"),
            ],
            Body::from("{}"),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let auth = upstream.state.auth_seen().await;
    assert!(
        auth.iter().any(|a| a == "x-api-key:dlk_test_key"),
        "upstream should see the client x-api-key: {auth:?}"
    );
    rig.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn upstream_api_key_replacement() {
    let (rig, upstream) = new_rig(|builder| {
        builder.server_config.tunnels[0].upstream_api_key = Some("sk-internal".into());
        builder.server_config.tunnels[0].upstream_auth_header = UpstreamAuth::XApiKey;
    })
    .await;
    let (status, _, body) = rig
        .request(
            "POST",
            "/home/v1/messages",
            &[
                ("authorization", "Bearer dlk_test_key"),
                ("content-type", "application/json"),
            ],
            Body::from("{}"),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let auth = upstream.state.auth_seen().await;
    assert!(
        auth.iter().any(|a| a == "x-api-key:sk-internal"),
        "expected the internal key to replace the client credential: {auth:?}"
    );
    rig.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn authentication_and_authorization() {
    let (rig, _upstream) = new_rig(|builder| {
        builder.server_config.tunnels.push(TunnelSettings {
            id: "other".into(),
            token: "agt_other".into(),
            mount: Some("/other".into()),
            hosts: vec![],
            default: false,
            protocols: vec![],
            upstream_api_key: None,
            upstream_auth_header: UpstreamAuth::Passthrough,
            public_paths: vec![],
            max_streams: None,
            request_timeout: None,
            enabled: true,
        });
        builder.server_config.api_keys.push(ApiKeySettings {
            name: "scoped".into(),
            key: "dlk_scoped".into(),
            allow_tunnels: vec!["other".into()],
            allow_ips: vec![],
            rate_limit: None,
            max_concurrent: 0,
        });
        builder.server_config.api_keys.push(ApiKeySettings {
            name: "limited".into(),
            key: "dlk_limited".into(),
            allow_tunnels: vec![],
            allow_ips: vec![],
            rate_limit: Some("2/min".into()),
            max_concurrent: 0,
        });
    })
    .await;
    let (status, _, _) = rig.get("/home/v1/models", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = rig.get("/home/v1/models", Some("Bearer wrong")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = rig.get("/home/v1/models", Some("Bearer dlk_scoped")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = rig.get("/home/v1/models", Some("Bearer dlk_limited")).await;
    assert_eq!(status, StatusCode::OK);
    let _ = rig.get("/home/v1/models", Some("Bearer dlk_limited")).await;
    let (status, headers, _) = rig.get("/home/v1/models", Some("Bearer dlk_limited")).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(headers.contains_key(header::RETRY_AFTER));
    // Public health endpoint needs no key.
    let (status, _, body) = rig.get("/home/health", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_ref(), b"ok");
    rig.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn large_body_integrity() {
    let (rig, _upstream) = new_rig(|_| {}).await;
    let payload: Vec<u8> = (0..8 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
    let expected = hex(&Sha256::digest(&payload));
    let (status, headers, body) = rig
        .request(
            "POST",
            "/home/echo",
            &[("authorization", "Bearer dlk_test_key")],
            Body::from(payload.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers.get("x-echo-sha256").unwrap().to_str().unwrap(),
        expected
    );
    assert_eq!(body.len(), payload.len());
    assert_eq!(body.as_ref(), payload.as_slice());
    rig.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_requests_are_isolated() {
    let (rig, _upstream) = new_rig(|_| {}).await;
    let mut handles = Vec::new();
    for i in 0..16u32 {
        let rig_client = rig.client.clone();
        let url = rig.https_url("/home/echo");
        handles.push(tokio::spawn(async move {
            let body = format!("request-{i}").repeat(64);
            let request = Request::builder()
                .method("POST")
                .uri(url)
                .header("authorization", "Bearer dlk_test_key")
                .body(Body::from(body.clone()))
                .unwrap();
            let response = rig_client.request(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            assert_eq!(bytes.as_ref(), body.as_bytes());
        }));
    }
    for handle in handles {
        handle.await.unwrap();
    }
    rig.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn client_disconnect_cancels_upstream() {
    let (rig, upstream) = new_rig(|_| {}).await;
    let request = Request::builder()
        .method("GET")
        .uri(rig.https_url("/home/sse"))
        .header("authorization", "Bearer dlk_test_key")
        .body(Body::empty())
        .unwrap();
    let response = rig.client.request(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();
    let first = tokio::time::timeout(Duration::from_secs(2), body.frame())
        .await
        .expect("first frame in time")
        .expect("frame")
        .unwrap();
    assert!(first.data_ref().is_some());
    assert!(upstream.state.inflight.load(Ordering::SeqCst) >= 1);
    drop(body);
    // The upstream request must be cancelled (inflight drops to 0) promptly.
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if upstream.state.inflight.load(Ordering::SeqCst) == 0 {
            rig.shutdown().await;
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("upstream request was not cancelled after the client disconnected");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reconnects_after_gateway_restart() {
    let (mut rig, _upstream) = new_rig(|_| {}).await;
    let (status, _, _) = rig
        .get("/home/v1/models", Some("Bearer dlk_test_key"))
        .await;
    assert_eq!(status, StatusCode::OK);

    rig.restart_gateway().await;
    // Wait for the agent to reconnect, then verify the tunnel still works.
    let (status, _, body) = rig
        .get("/home/v1/models", Some("Bearer dlk_test_key"))
        .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let status = rig.admin_get("/status").await.unwrap();
    assert_eq!(status.0, StatusCode::OK);
    let value: Value = serde_json::from_slice(&status.2).unwrap();
    assert!(value["tunnels"][0]["connected"].as_bool().unwrap());
    rig.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn admin_endpoints() {
    let (rig, _upstream) = new_rig(|_| {}).await;

    let (status, _, body) = rig.admin_get("/healthz").await.unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_ref(), b"ok\n");

    let (status, _, body) = rig.admin_get("/readyz").await.unwrap();
    assert_eq!(status, StatusCode::OK);
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["ready"], true);
    assert_eq!(value["connected"], 1);

    let (status, _, body) = rig.admin_get("/metrics").await.unwrap();
    assert_eq!(status, StatusCode::OK);
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("divertlink_requests_total"), "{text}");
    assert!(
        text.contains("divertlink_tunnel_connections_total"),
        "{text}"
    );

    let (status, _, body) = rig.admin_get("/status").await.unwrap();
    assert_eq!(status, StatusCode::OK);
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["server"]["name"], "test-gateway");
    assert_eq!(value["tunnels"][0]["connected"], true);
    rig.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn static_route_proxies_to_newapi_like_upstream() {
    let upstream = start_mock_upstream().await;
    let tmp = TempDir::new().unwrap();
    let certs = tls::generate_certs(&GenCertsOptions {
        dir: tmp.path().join("certs"),
        domain: "localhost".into(),
        client_cert: false,
        self_signed: false,
        force: false,
    })
    .unwrap();
    let port = reserve_port();
    let mut builder = RigBuilder::new(upstream.addr, certs.ca_cert.clone(), port);
    builder.server_config.tls.cert = certs.server_cert.clone();
    builder.server_config.tls.key = certs.server_key.clone();
    builder.server_config.static_routes = vec![StaticRouteSettings {
        mount: "/newapi".into(),
        upstream: format!("http://{}", upstream.addr),
        strip_prefix: true,
        require_auth: true,
        tls_ca: None,
        headers: Default::default(),
    }];
    let rig = Rig::start_with(builder.build(), tmp.path().to_path_buf()).await;
    let (status, _, body) = rig
        .get("/newapi/v1/models", Some("Bearer dlk_test_key"))
        .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["data"][0]["id"], "mock-model");
    rig.shutdown().await;
    std::mem::forget(tmp);
    upstream.shutdown.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn static_route_auth_and_opt_out() {
    let upstream = start_mock_upstream().await;
    let tmp = TempDir::new().unwrap();
    let certs = tls::generate_certs(&GenCertsOptions {
        dir: tmp.path().join("certs"),
        domain: "localhost".into(),
        client_cert: false,
        self_signed: false,
        force: false,
    })
    .unwrap();
    let port = reserve_port();
    let mut builder = RigBuilder::new(upstream.addr, certs.ca_cert.clone(), port);
    builder.server_config.tls.cert = certs.server_cert.clone();
    builder.server_config.tls.key = certs.server_key.clone();
    builder.server_config.static_routes = vec![
        StaticRouteSettings {
            mount: "/api".into(),
            upstream: format!("http://{}", upstream.addr),
            strip_prefix: true,
            require_auth: true,
            tls_ca: None,
            headers: Default::default(),
        },
        StaticRouteSettings {
            mount: "/open".into(),
            upstream: format!("http://{}", upstream.addr),
            strip_prefix: true,
            require_auth: false,
            tls_ca: None,
            headers: Default::default(),
        },
    ];
    let rig = Rig::start_with(builder.build(), tmp.path().to_path_buf()).await;
    // Auth-required route: rejected without a key, accepted with one.
    let (status, _, _) = rig.get("/api/v1/models", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, body) = rig.get("/api/v1/models", Some("Bearer dlk_test_key")).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    // Opt-out route: reachable without a gateway key (upstream authenticates).
    let (status, _, body) = rig.get("/open/v1/models", None).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    rig.shutdown().await;
    upstream.shutdown.cancel();
    std::mem::forget(tmp);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plaintext_only_gateway() {
    // A gateway with only the plaintext listener and a static route: no TLS
    // material is needed at all (put nginx/Caddy in front in production).
    let upstream = start_mock_upstream().await;
    let tmp = TempDir::new().unwrap();
    let mut config = ServerConfig::default();
    config.server.name = "plain-gateway".into();
    config.server.tunnel_listen = None;
    config.server.ingress_listen = None;
    config.server.http_listen = Some(SocketAddr::from(([127, 0, 0, 1], 0)));
    config.server.admin_listen = Some(SocketAddr::from(([127, 0, 0, 1], 0)));
    config.static_routes = vec![StaticRouteSettings {
        mount: "/api".into(),
        upstream: format!("http://{}", upstream.addr),
        strip_prefix: true,
        require_auth: false,
        tls_ca: None,
        headers: Default::default(),
    }];
    let gateway = server::run(config, None)
        .await
        .expect("plaintext gateway starts");
    let addrs = gateway.addrs();
    let http = addrs.http.expect("http listener");
    let admin = addrs.admin.expect("admin listener");
    let client = upstream::build_http_client(None, Duration::from_secs(5)).unwrap();
    let request = Request::builder()
        .uri(format!("http://{http}/api/v1/models"))
        .body(Body::empty())
        .unwrap();
    let response = client.request(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["data"][0]["id"], "mock-model");
    let ready = client
        .request(
            Request::builder()
                .uri(format!("http://{admin}/readyz"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // No tunnels configured => trivially ready.
    assert_eq!(ready.status(), StatusCode::OK);
    gateway.shutdown().await;
    upstream.shutdown.cancel();
    std::mem::forget(tmp);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_routing_and_default_tunnel() {
    let (rig, _upstream) = new_rig(|builder| {
        builder.server_config.tunnels.push(TunnelSettings {
            id: "vhost".into(),
            token: "agt_vhost".into(),
            mount: None,
            hosts: vec!["llm.example.com".into()],
            default: false,
            protocols: vec![],
            upstream_api_key: None,
            upstream_auth_header: UpstreamAuth::Passthrough,
            public_paths: vec![],
            max_streams: None,
            request_timeout: None,
            enabled: true,
        });
    })
    .await;
    // The vhost tunnel has no agent connected: requests get 503, not 404.
    let request = Request::builder()
        .uri(rig.https_url("/v1/models"))
        .header("host", "llm.example.com")
        .header("authorization", "Bearer dlk_test_key")
        .body(Body::empty())
        .unwrap();
    let response = rig.client.request(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    rig.shutdown().await;
}

// ---------------------------------------------------------------------------
// TLS security properties
// ---------------------------------------------------------------------------

/// Start gateway + agent with fully custom TLS configuration; returns the rig and
/// whether the tunnel became ready within the timeout.
async fn start_custom_tls(
    server_tweaks: impl FnOnce(&mut ServerConfig, &tls::CertReport, &mut AgentConfig),
    agent_tweaks: impl FnOnce(&mut AgentConfig),
    certs: &tls::CertReport,
    upstream: SocketAddr,
    tunnel_port: u16,
    ingress_port: u16,
) -> (Rig, bool) {
    init_tracing();
    let mut builder = RigBuilder::new(upstream, certs.ca_cert.clone(), ingress_port);
    builder.server_config.tls.cert = certs.server_cert.clone();
    builder.server_config.tls.key = certs.server_key.clone();
    builder.server_config.server.tunnel_listen =
        Some(SocketAddr::from(([127, 0, 0, 1], tunnel_port)));
    builder.server_config.server.ingress_listen =
        Some(SocketAddr::from(([127, 0, 0, 1], ingress_port)));
    let mut server_config = builder.server_config;
    let mut agent_config = builder.agent_config;
    server_tweaks(&mut server_config, certs, &mut agent_config);
    agent_tweaks(&mut agent_config);
    let ca = agent_config.tls.ca.clone().expect("CA");
    let client = upstream::build_http_client(Some(&ca), Duration::from_secs(5)).unwrap();
    let gateway = server::run(server_config.clone(), None)
        .await
        .expect("gateway starts");
    let addrs = gateway.addrs();
    let ingress_port = addrs.ingress.expect("ingress").port();
    let admin = addrs.admin.expect("admin");
    agent_config.agent.server = format!("127.0.0.1:{tunnel_port}");
    let agent = agent::run(agent_config, None).await.expect("agent starts");
    let rig = Rig {
        _tmp: TempDir::new().unwrap(),
        ingress_port,
        admin,
        gateway: Some(gateway),
        agent: Some(agent),
        client,
        ca,
        server_config,
        dead: false,
    };
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut ready = false;
    while Instant::now() < deadline {
        if let Ok((status, _, _)) = rig.admin_get("/readyz").await {
            if status == StatusCode::OK {
                ready = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    (rig, ready)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mtls_requires_agent_client_certificate() {
    let upstream = start_mock_upstream().await;
    let tmp = TempDir::new().unwrap();
    let certs = tls::generate_certs(&GenCertsOptions {
        dir: tmp.path().join("certs"),
        domain: "localhost".into(),
        client_cert: true,
        self_signed: false,
        force: false,
    })
    .unwrap();
    let agent_cert = certs.client_cert.clone().unwrap();
    let agent_key = certs.client_key.clone().unwrap();

    // (a) Agent without a client certificate: the gateway must reject it.
    let (rig_bad, ready) = start_custom_tls(
        |server, certs, _agent| {
            server.tls.client_ca = Some(certs.ca_cert.clone());
        },
        |_agent| {},
        &certs,
        upstream.addr,
        reserve_port(),
        reserve_port(),
    )
    .await;
    assert!(
        !ready,
        "agent without a client certificate must not connect (mTLS)"
    );
    assert!(
        rig_bad.wait_not_ready(Duration::from_secs(1)).await,
        "gateway must stay not-ready without an authenticated agent"
    );
    rig_bad.shutdown().await;

    // (b) Agent with the generated client certificate: connects and works.
    let (rig_good, ready) = start_custom_tls(
        |server, certs, _agent| {
            server.tls.client_ca = Some(certs.ca_cert.clone());
        },
        move |agent| {
            agent.tls.client_cert = Some(agent_cert);
            agent.tls.client_key = Some(agent_key);
        },
        &certs,
        upstream.addr,
        reserve_port(),
        reserve_port(),
    )
    .await;
    assert!(
        ready,
        "agent with a valid client certificate should connect"
    );
    let (status, _, body) = rig_good
        .get("/home/v1/models", Some("Bearer dlk_test_key"))
        .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    rig_good.shutdown().await;
    upstream.shutdown.cancel();
    std::mem::forget(tmp);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spki_pin_mismatch_blocks_agent() {
    let upstream = start_mock_upstream().await;
    let tmp = TempDir::new().unwrap();
    let certs = tls::generate_certs(&GenCertsOptions {
        dir: tmp.path().join("certs"),
        domain: "localhost".into(),
        client_cert: false,
        self_signed: false,
        force: false,
    })
    .unwrap();
    let (rig, ready) = start_custom_tls(
        |_server, _certs, _agent| {},
        |agent| {
            // A pin that cannot match the gateway certificate (all zero digest,
            // base64url of 32 bytes).
            agent.tls.pin = Some("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into());
        },
        &certs,
        upstream.addr,
        reserve_port(),
        reserve_port(),
    )
    .await;
    assert!(
        !ready,
        "agent must refuse to connect when the SPKI pin does not match"
    );
    rig.shutdown().await;
    upstream.shutdown.cancel();
    std::mem::forget(tmp);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spki_pin_match_allows_agent() {
    let upstream = start_mock_upstream().await;
    let tmp = TempDir::new().unwrap();
    let certs = tls::generate_certs(&GenCertsOptions {
        dir: tmp.path().join("certs"),
        domain: "localhost".into(),
        client_cert: false,
        self_signed: false,
        force: false,
    })
    .unwrap();
    let pin = certs.server_pin.clone();
    let (rig, ready) = start_custom_tls(
        |_server, _certs, _agent| {},
        move |agent| agent.tls.pin = Some(pin),
        &certs,
        upstream.addr,
        reserve_port(),
        reserve_port(),
    )
    .await;
    assert!(ready, "a correct SPKI pin must allow the agent to connect");
    let (status, _, body) = rig
        .get("/home/v1/models", Some("Bearer dlk_test_key"))
        .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    rig.shutdown().await;
    upstream.shutdown.cancel();
    std::mem::forget(tmp);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wrong_agent_token_is_rejected() {
    let upstream = start_mock_upstream().await;
    let tmp = TempDir::new().unwrap();
    let certs = tls::generate_certs(&GenCertsOptions {
        dir: tmp.path().join("certs"),
        domain: "localhost".into(),
        client_cert: false,
        self_signed: false,
        force: false,
    })
    .unwrap();
    let (rig, ready) = start_custom_tls(
        |_server, _certs, _agent| {},
        |agent| {
            agent.agent.token = "agt_wrong_token".into();
        },
        &certs,
        upstream.addr,
        reserve_port(),
        reserve_port(),
    )
    .await;
    assert!(!ready, "an agent with a wrong token must not register");
    rig.shutdown().await;
    upstream.shutdown.cancel();
    std::mem::forget(tmp);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn http2_ingress_works() {
    let (rig, _upstream) = new_rig(|_| {}).await;
    // Build a client that negotiates HTTP/2 via ALPN.
    let tls_config = divertlink::tls::build_https_client_config(Some(&std::path::PathBuf::from(
        rig.cert_path(),
    )))
    .unwrap();
    let mut connector = HttpConnector::new();
    connector.enforce_http(false);
    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config((*tls_config).clone())
        .https_or_http()
        .enable_http2()
        .build();
    let client: Client<_, Body> =
        Client::builder(hyper_util::rt::TokioExecutor::new()).build(https);
    let request = Request::builder()
        .uri(rig.https_url("/home/v1/models"))
        .header("authorization", "Bearer dlk_test_key")
        .body(Body::empty())
        .unwrap();
    let response = client.request(request).await.expect("h2 request");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.version(), http::Version::HTTP_2);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["data"][0]["id"], "mock-model");
    rig.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn method_allowlist_and_global_rate_limit() {
    // Only GET/POST allowed, gateway-wide rate limit of 2 requests per minute.
    let (rig, _upstream) = new_rig(|builder| {
        builder.server_config.limits.allowed_methods = vec!["GET".to_string(), "POST".to_string()];
        builder.server_config.limits.global_rate_limit = Some("2/min".to_string());
    })
    .await;

    // TRACE is not in the allow list and must be rejected before authentication.
    let request = Request::builder()
        .method("TRACE")
        .uri(rig.https_url("/home/v1/models"))
        .body(Body::empty())
        .unwrap();
    let response = rig.client.request(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    let allow = response
        .headers()
        .get(header::ALLOW)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(allow.contains("GET") && allow.contains("POST"), "{allow}");

    // The global limiter allows the first two requests, then returns 429.
    let (status, _, _) = rig
        .get("/home/v1/models", Some("Bearer dlk_test_key"))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = rig
        .get("/home/v1/models", Some("Bearer dlk_test_key"))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, headers, _) = rig
        .get("/home/v1/models", Some("Bearer dlk_test_key"))
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(headers.contains_key(header::RETRY_AFTER));
    rig.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sse_responses_are_marked_unbuffered() {
    let (rig, _upstream) = new_rig(|_| {}).await;
    let request = Request::builder()
        .method("POST")
        .uri(rig.https_url("/home/v1/chat/completions"))
        .header("authorization", "Bearer dlk_test_key")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&json!({"model": "mock-model", "stream": true})).unwrap(),
        ))
        .unwrap();
    let response = rig.client.request(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("x-accel-buffering").unwrap(),
        "no",
        "SSE responses must tell proxies not to buffer"
    );
    let cache_control = response
        .headers()
        .get(header::CACHE_CONTROL)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(cache_control.contains("no-transform"), "{cache_control}");
    let _ = response.into_body().collect().await.unwrap();
    rig.shutdown().await;
}
