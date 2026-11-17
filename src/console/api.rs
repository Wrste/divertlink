//! Console REST API + live log stream.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use axum::middleware::Next;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use futures_util::stream::Stream;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use super::probe::{self, ProbeOptions, ProbeReport};
use super::profile::{self, summarize_config, ConfigSummary, Profile, ProfileRole, ProfileStore};
use super::supervisor::{LogLine, ProcStatus, Supervisor};
use crate::error::Result;
use crate::upstream::HttpClient;

/// Shared console state.
pub struct ConsoleState {
    /// Profile store.
    pub store: Arc<ProfileStore>,
    /// Process supervisor.
    pub supervisor: Arc<Supervisor>,
    /// Process start time (UNIX seconds).
    pub started_unix: u64,
    /// Bound console address.
    pub listen: SocketAddr,
    /// Optional access token (required when listening beyond loopback).
    pub token: Option<String>,
    /// Pooled HTTP client used to poll gateway admin endpoints.
    pub http: HttpClient,
    /// Cancels the console server (and stops children).
    pub shutdown: CancellationToken,
}

impl std::fmt::Debug for ConsoleState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConsoleState")
            .field("listen", &self.listen)
            .field("token", &self.token.is_some())
            .finish()
    }
}

/// Build the console router (API + embedded UI).
pub fn router(state: Arc<ConsoleState>) -> Router {
    Router::new()
        .route("/api/version", get(version))
        .route("/api/state", get(state_handler))
        .route("/api/healthz", get(healthz))
        .route("/api/profiles", get(list_profiles).post(create_profile))
        .route(
            "/api/profiles/{id}",
            patch(update_profile).delete(delete_profile),
        )
        .route("/api/profiles/{id}/start", post(start_profile))
        .route("/api/profiles/{id}/stop", post(stop_profile))
        .route("/api/profiles/{id}/restart", post(restart_profile))
        .route("/api/profiles/{id}/status", get(profile_status))
        .route("/api/profiles/{id}/config", get(get_config).put(put_config))
        .route("/api/profiles/{id}/logs", get(stream_logs))
        .route("/api/profiles/{id}/logs/history", get(log_history))
        .route("/api/logs/history", get(all_log_history))
        .route("/api/logs", get(stream_all_logs))
        .route("/api/init", post(init_project))
        .route("/api/token", post(generate_token))
        .route("/api/probe", post(probe_endpoint))
        .route("/api/export", get(export_store))
        .route("/api/import", post(import_store))
        .route("/api/select", post(select_profile))
        .route("/api/shutdown", post(shutdown_console))
        .route("/", get(super::assets::index))
        .route("/index.html", get(super::assets::index))
        .route("/app.js", get(super::assets::app_js))
        .route("/style.css", get(super::assets::style_css))
        .route("/manifest.webmanifest", get(super::assets::manifest))
        .route("/sw.js", get(super::assets::service_worker))
        .route("/icon.svg", get(super::assets::icon))
        .route("/favicon.ico", get(super::assets::icon))
        .fallback(super::assets::not_found)
        .layer(axum::middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
}

/// Security middleware: loopback-only by default, anti-CSRF and optional token.
async fn guard(
    State(state): State<Arc<ConsoleState>>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let path = request.uri().path().to_string();
    if !path.starts_with("/api") {
        return next.run(request).await;
    }
    // 1) Host allow list prevents DNS-rebinding attacks against a loopback console.
    if state.listen.ip().is_loopback() {
        let host = request
            .headers()
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .map(crate::config::host_without_port)
            .unwrap_or_default();
        let allowed =
            host == "localhost" || host == "127.0.0.1" || host == "::1" || host.is_empty();
        if !allowed {
            return error(
                StatusCode::FORBIDDEN,
                "host_not_allowed",
                "unexpected Host header",
            );
        }
    }
    // 2) Optional bearer token.
    if let Some(expected) = &state.token {
        let presented = request
            .headers()
            .get("x-console-token")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
            .or_else(|| {
                request.uri().query().and_then(|q| {
                    q.split('&')
                        .find_map(|pair| pair.strip_prefix("token=").map(str::to_string))
                })
            });
        match presented {
            Some(token) if crate::auth::constant_time_eq(&token, expected) => {}
            _ => {
                return error(
                    StatusCode::UNAUTHORIZED,
                    "console_token_required",
                    "a console token is required (x-console-token or ?token=)",
                )
            }
        }
    }
    // 3) Mutations must carry a custom header: browsers cannot send it cross-origin
    //    without a CORS preflight, which this server never approves.
    let method = request.method();
    if matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    ) {
        let has_header = request
            .headers()
            .get("x-divertlink-console")
            .map(|v| v == "1")
            .unwrap_or(false);
        if !has_header {
            return error(
                StatusCode::FORBIDDEN,
                "missing_console_header",
                "mutating requests must include `x-divertlink-console: 1`",
            );
        }
    }
    next.run(request).await
}

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({"ok": false, "error": {"code": code, "message": message}})),
    )
        .into_response()
}

fn internal(err: impl std::fmt::Display) -> Response {
    error(StatusCode::BAD_REQUEST, "internal", &err.to_string())
}

#[derive(Debug, Serialize)]
struct ApiOk<T: Serialize> {
    ok: bool,
    data: T,
}

fn ok<T: Serialize>(data: T) -> Response {
    Json(ApiOk { ok: true, data }).into_response()
}

async fn version() -> Response {
    ok(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "protocol": crate::protocol::PROTOCOL_VERSION,
        "kind": "divertlink-console",
    }))
}

async fn healthz() -> Response {
    (StatusCode::OK, "ok\n").into_response()
}

#[derive(Serialize)]
struct ProfileView {
    #[serde(flatten)]
    profile: Profile,
    status: ProcStatus,
    summary: Option<ConfigSummary>,
    exists: bool,
}

#[derive(Serialize)]
struct StateView {
    version: String,
    home: String,
    store: String,
    listen: String,
    started_unix: u64,
    token_required: bool,
    selected: Option<String>,
    profiles: Vec<ProfileView>,
}

async fn state_handler(State(state): State<Arc<ConsoleState>>) -> Response {
    let profiles = state
        .store
        .list()
        .into_iter()
        .map(|profile| {
            let summary = summarize_config(&profile.config).ok();
            ProfileView {
                status: state.supervisor.status(&profile.id),
                exists: profile.config.exists(),
                summary,
                profile,
            }
        })
        .collect();
    ok(StateView {
        version: env!("CARGO_PKG_VERSION").to_string(),
        home: profile::home_dir().display().to_string(),
        store: state.store.path().display().to_string(),
        listen: state.listen.to_string(),
        started_unix: state.started_unix,
        token_required: state.token.is_some(),
        selected: state.store.selected(),
        profiles,
    })
}

async fn list_profiles(State(state): State<Arc<ConsoleState>>) -> Response {
    let profiles: Vec<ProfileView> = state
        .store
        .list()
        .into_iter()
        .map(|profile| ProfileView {
            status: state.supervisor.status(&profile.id),
            summary: summarize_config(&profile.config).ok(),
            exists: profile.config.exists(),
            profile,
        })
        .collect();
    ok(profiles)
}

#[derive(Deserialize)]
struct CreateProfile {
    name: String,
    #[serde(default)]
    role: Option<String>,
    config: String,
    #[serde(default)]
    autostart: bool,
    #[serde(default)]
    notes: String,
}

async fn create_profile(
    State(state): State<Arc<ConsoleState>>,
    Json(body): Json<CreateProfile>,
) -> Response {
    let config = PathBuf::from(body.config.trim());
    let role = match body.role.as_deref().and_then(ProfileRole::parse) {
        Some(role) => role,
        None => match profile::detect_role(&config) {
            Ok(role) => role,
            Err(err) => return internal(err),
        },
    };
    if let Err(err) = summarize_config(&config) {
        return internal(err);
    }
    let profile = Profile {
        id: String::new(),
        name: body.name,
        role,
        config,
        autostart: body.autostart,
        notes: body.notes,
        created_unix: 0,
    };
    match state.store.upsert(profile) {
        Ok(profile) => ok(profile),
        Err(err) => internal(err),
    }
}

#[derive(Deserialize, Default)]
struct UpdateProfile {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    config: Option<String>,
    #[serde(default)]
    autostart: Option<bool>,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    role: Option<String>,
}

async fn update_profile(
    State(state): State<Arc<ConsoleState>>,
    Path(id): Path<String>,
    Json(body): Json<UpdateProfile>,
) -> Response {
    let Some(mut profile) = state.store.get(&id) else {
        return error(StatusCode::NOT_FOUND, "not_found", "profile not found");
    };
    if let Some(name) = body.name {
        profile.name = name;
    }
    if let Some(config) = body.config {
        profile.config = PathBuf::from(config);
    }
    if let Some(autostart) = body.autostart {
        profile.autostart = autostart;
    }
    if let Some(notes) = body.notes {
        profile.notes = notes;
    }
    if let Some(role) = body.role.as_deref().and_then(ProfileRole::parse) {
        profile.role = role;
    }
    if let Err(err) = summarize_config(&profile.config) {
        return internal(err);
    }
    match state.store.upsert(profile) {
        Ok(profile) => ok(profile),
        Err(err) => internal(err),
    }
}

async fn delete_profile(
    State(state): State<Arc<ConsoleState>>,
    Path(id): Path<String>,
) -> Response {
    let _ = state.supervisor.stop(&id).await;
    match state.store.remove(&id) {
        Ok(()) => ok(json!({"removed": id})),
        Err(err) => internal(err),
    }
}

async fn start_profile(State(state): State<Arc<ConsoleState>>, Path(id): Path<String>) -> Response {
    let Some(profile) = state.store.get(&id) else {
        return error(StatusCode::NOT_FOUND, "not_found", "profile not found");
    };
    match state.supervisor.start(&profile).await {
        Ok(status) => ok(status),
        Err(err) => internal(err),
    }
}

async fn stop_profile(State(state): State<Arc<ConsoleState>>, Path(id): Path<String>) -> Response {
    match state.supervisor.stop(&id).await {
        Ok(status) => ok(status),
        Err(err) => internal(err),
    }
}

async fn restart_profile(
    State(state): State<Arc<ConsoleState>>,
    Path(id): Path<String>,
) -> Response {
    let Some(profile) = state.store.get(&id) else {
        return error(StatusCode::NOT_FOUND, "not_found", "profile not found");
    };
    match state.supervisor.restart(&profile).await {
        Ok(status) => ok(status),
        Err(err) => internal(err),
    }
}

#[derive(Serialize)]
struct LiveStatus {
    process: ProcStatus,
    summary: Option<ConfigSummary>,
    admin: Option<serde_json::Value>,
    ready: Option<bool>,
    logs_tail: Vec<LogLine>,
}

async fn get_json(client: &HttpClient, url: &str) -> Option<serde_json::Value> {
    let uri = url.parse::<axum::http::Uri>().ok()?;
    let request = axum::http::Request::builder()
        .uri(uri)
        .body(axum::body::Body::empty())
        .ok()?;
    let response = client.request(request).await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let bytes = http_body_util::BodyExt::collect(response.into_body())
        .await
        .ok()?
        .to_bytes();
    serde_json::from_slice(&bytes).ok()
}

async fn profile_status(
    State(state): State<Arc<ConsoleState>>,
    Path(id): Path<String>,
) -> Response {
    let Some(profile) = state.store.get(&id) else {
        return error(StatusCode::NOT_FOUND, "not_found", "profile not found");
    };
    let summary = summarize_config(&profile.config).ok();
    let mut admin = None;
    let mut ready = None;
    if profile.role == ProfileRole::Server {
        if let Some(admin_addr) = summary
            .as_ref()
            .and_then(|s| s.listeners.get("admin"))
            .cloned()
        {
            let base = format!("http://{admin_addr}");
            admin = get_json(&state.http, &format!("{base}/status")).await;
            if let Ok(uri) = format!("{base}/readyz").parse::<axum::http::Uri>() {
                if let Ok(response) = state
                    .http
                    .request(
                        axum::http::Request::builder()
                            .uri(uri)
                            .body(axum::body::Body::empty())
                            .unwrap_or_default(),
                    )
                    .await
                {
                    ready = Some(response.status().is_success());
                }
            }
        }
    }
    let logs_tail = state.supervisor.logs().history(0, Some(&id), 200);
    ok(LiveStatus {
        process: state.supervisor.status(&id),
        summary,
        admin,
        ready,
        logs_tail,
    })
}

#[derive(Deserialize)]
struct HistoryQuery {
    #[serde(default)]
    after: u64,
    #[serde(default)]
    limit: Option<usize>,
}

async fn log_history(
    State(state): State<Arc<ConsoleState>>,
    Path(id): Path<String>,
    Query(query): Query<HistoryQuery>,
) -> Response {
    let lines = state.supervisor.logs().history(
        query.after,
        Some(&id),
        query.limit.unwrap_or(500).min(2000),
    );
    ok(lines)
}

async fn all_log_history(
    State(state): State<Arc<ConsoleState>>,
    Query(query): Query<HistoryQuery>,
) -> Response {
    let lines =
        state
            .supervisor
            .logs()
            .history(query.after, None, query.limit.unwrap_or(500).min(2000));
    ok(lines)
}

#[derive(Deserialize)]
struct StreamQuery {
    #[serde(default)]
    after: u64,
}

async fn stream_logs(
    State(state): State<Arc<ConsoleState>>,
    Path(id): Path<String>,
    Query(query): Query<StreamQuery>,
) -> Sse<impl Stream<Item = std::result::Result<Event, Infallible>>> {
    sse_logs(state, Some(id), query.after)
}

async fn stream_all_logs(
    State(state): State<Arc<ConsoleState>>,
    Query(query): Query<StreamQuery>,
) -> Sse<impl Stream<Item = std::result::Result<Event, Infallible>>> {
    sse_logs(state, None, query.after)
}

fn sse_logs(
    state: Arc<ConsoleState>,
    profile: Option<String>,
    after: u64,
) -> Sse<impl Stream<Item = std::result::Result<Event, Infallible>>> {
    let logs = state.supervisor.logs();
    let receiver = logs.subscribe();
    let backlog: Vec<LogLine> = logs.history(after, profile.as_deref(), 2000);
    let stream =
        futures_util::stream::unfold((receiver, backlog), move |(mut receiver, mut backlog)| {
            let profile = profile.clone();
            async move {
                // Send the backlog first.
                if !backlog.is_empty() {
                    let line = backlog.remove(0);
                    let event = Event::default()
                        .event("log")
                        .json_data(line)
                        .unwrap_or_else(|_| Event::default().comment("encoding error"));
                    return Some((Ok(event), (receiver, backlog)));
                }
                loop {
                    match receiver.recv().await {
                        Ok(line) => {
                            if let Some(filter) = &profile {
                                if &line.profile != filter {
                                    continue;
                                }
                            }
                            let event = Event::default()
                                .event("log")
                                .json_data(line)
                                .unwrap_or_else(|_| Event::default().comment("encoding error"));
                            return Some((Ok(event), (receiver, backlog)));
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => return None,
                    }
                }
            }
        });
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

async fn get_config(State(state): State<Arc<ConsoleState>>, Path(id): Path<String>) -> Response {
    let Some(profile) = state.store.get(&id) else {
        return error(StatusCode::NOT_FOUND, "not_found", "profile not found");
    };
    let content = match std::fs::read_to_string(&profile.config) {
        Ok(content) => content,
        Err(err) => {
            return error(
                StatusCode::NOT_FOUND,
                "config_unreadable",
                &format!("cannot read {}: {err}", profile.config.display()),
            )
        }
    };
    ok(json!({
        "path": profile.config.display().to_string(),
        "content": content,
        "summary": summarize_config(&profile.config).ok(),
    }))
}

#[derive(Deserialize)]
struct PutConfig {
    content: String,
}

async fn put_config(
    State(state): State<Arc<ConsoleState>>,
    Path(id): Path<String>,
    Json(body): Json<PutConfig>,
) -> Response {
    let Some(profile) = state.store.get(&id) else {
        return error(StatusCode::NOT_FOUND, "not_found", "profile not found");
    };
    let validation: Result<()> = match profile.role {
        ProfileRole::Server => {
            crate::config::ServerConfig::from_toml_str(&body.content).map(|_| ())
        }
        ProfileRole::Agent => crate::config::AgentConfig::from_toml_str(&body.content).map(|_| ()),
    };
    if let Err(err) = validation {
        return internal(err);
    }
    // Keep one backup so a bad edit can be recovered by hand.
    if profile.config.exists() {
        let backup = profile.config.with_extension("toml.bak");
        let _ = std::fs::copy(&profile.config, &backup);
    }
    if let Err(err) = std::fs::write(&profile.config, body.content.as_bytes()) {
        return internal(format!("cannot write config: {err}"));
    }
    state.supervisor.logs().push(
        &id,
        "console",
        format!("config saved: {}", profile.config.display()),
    );
    ok(json!({
        "path": profile.config.display().to_string(),
        "summary": summarize_config(&profile.config).ok(),
    }))
}

#[derive(Deserialize)]
struct InitRequest {
    dir: String,
    domain: String,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    mtls: bool,
    #[serde(default)]
    force: bool,
}

async fn init_project(
    State(state): State<Arc<ConsoleState>>,
    Json(body): Json<InitRequest>,
) -> Response {
    let role = body
        .role
        .as_deref()
        .and_then(crate::setup::InitRole::parse)
        .unwrap_or(crate::setup::InitRole::Both);
    let domain = body.domain.clone();
    let options = crate::setup::InitOptions {
        dir: PathBuf::from(&body.dir),
        domain: body.domain,
        role,
        mtls: body.mtls,
        force: body.force,
    };
    let result = tokio::task::spawn_blocking(move || crate::setup::init_project(&options)).await;
    let report = match result {
        Ok(Ok(report)) => report,
        Ok(Err(err)) => return internal(err),
        Err(err) => return internal(format!("init task failed: {err}")),
    };
    let mut created: Vec<Profile> = Vec::new();
    let mut register = |path: &PathBuf, role: ProfileRole, name: String| {
        let profile = Profile {
            id: String::new(),
            name,
            role,
            config: path.clone(),
            autostart: false,
            notes: String::new(),
            created_unix: 0,
        };
        match state.store.upsert(profile) {
            Ok(profile) => created.push(profile),
            Err(err) => state.supervisor.logs().push(
                "_console",
                "console",
                format!("cannot register generated profile: {err}"),
            ),
        }
    };
    if let Some(path) = &report.server_toml {
        register(path, ProfileRole::Server, format!("网关 {domain}"));
    }
    if let Some(path) = &report.agent_toml {
        register(path, ProfileRole::Agent, format!("家庭端 {domain}"));
    }
    ok(json!({
        "server_toml": report.server_toml.as_ref().map(|p| p.display().to_string()),
        "agent_toml": report.agent_toml.as_ref().map(|p| p.display().to_string()),
        "api_key": report.api_key,
        "agent_token": report.agent_token,
        "server_pin": report.certs.as_ref().map(|c| c.server_pin.clone()),
        "ca_cert": report.certs.as_ref().map(|c| c.ca_cert.display().to_string()),
        "profiles": created,
    }))
}

#[derive(Deserialize)]
struct TokenRequest {
    #[serde(default)]
    prefix: Option<String>,
    #[serde(default)]
    bytes: Option<usize>,
}

async fn generate_token(Json(body): Json<TokenRequest>) -> Response {
    let prefix = body.prefix.unwrap_or_else(|| "dlk_".to_string());
    let bytes = body.bytes.unwrap_or(24).clamp(8, 64);
    ok(json!({"token": crate::util::random_token(&prefix, bytes)}))
}

#[derive(Deserialize)]
struct ProbeRequest {
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    endpoint: Option<String>,
    #[serde(default)]
    ca: Option<String>,
    #[serde(default)]
    insecure: bool,
    #[serde(default)]
    pin: Option<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

async fn probe_endpoint(
    State(state): State<Arc<ConsoleState>>,
    Json(body): Json<ProbeRequest>,
) -> Response {
    let report: ProbeReport = if let Some(id) = &body.profile {
        let Some(profile) = state.store.get(id) else {
            return error(StatusCode::NOT_FOUND, "not_found", "profile not found");
        };
        match probe::probe_config(&profile.config).await {
            Ok(report) => report,
            Err(err) => return internal(err),
        }
    } else {
        let Some(endpoint) = body.endpoint.clone() else {
            return error(
                StatusCode::BAD_REQUEST,
                "missing_endpoint",
                "provide `profile` or `endpoint` (host:port)",
            );
        };
        let (host, port) = match probe::parse_endpoint(&endpoint, 443) {
            Ok(pair) => pair,
            Err(err) => return internal(err),
        };
        let options = ProbeOptions {
            host,
            port,
            servername: None,
            ca: body.ca.as_ref().map(PathBuf::from),
            insecure: body.insecure,
            pin: body.pin.clone(),
            timeout: Duration::from_millis(body.timeout_ms.unwrap_or(8000).clamp(500, 30_000)),
        };
        probe::probe_gateway(&options).await
    };
    ok(report)
}

async fn export_store(State(state): State<Arc<ConsoleState>>) -> Response {
    match state.store.export_json() {
        Ok(json) => ok(json!({"profiles": json})),
        Err(err) => internal(err),
    }
}

#[derive(Deserialize)]
struct ImportRequest {
    profiles: String,
}

async fn import_store(
    State(state): State<Arc<ConsoleState>>,
    Json(body): Json<ImportRequest>,
) -> Response {
    match state.store.import_json(&body.profiles) {
        Ok(count) => ok(json!({"imported": count})),
        Err(err) => internal(err),
    }
}

#[derive(Deserialize)]
struct SelectRequest {
    #[serde(default)]
    id: Option<String>,
}

async fn select_profile(
    State(state): State<Arc<ConsoleState>>,
    Json(body): Json<SelectRequest>,
) -> Response {
    match state.store.select(body.id) {
        Ok(()) => ok(json!({"selected": state.store.selected()})),
        Err(err) => internal(err),
    }
}

async fn shutdown_console(State(state): State<Arc<ConsoleState>>) -> Response {
    debug!("console shutdown requested via API");
    state.shutdown.cancel();
    ok(json!({"shutting_down": true}))
}

/// Response helper used by the embedded asset handlers.
pub fn asset_response(content_type: &'static str, body: &'static str, cache: bool) -> Response {
    let mut response = body.into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, content_type.parse().unwrap());
    if !cache {
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    }
    response
}

/// Silence unused imports in non-test builds.
#[allow(dead_code)]
fn _markers(_: HashMap<String, String>, _: HeaderMap) {}
