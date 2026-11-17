//! Tunnel registry: static route table, session installation/replacement, route
//! resolution and stream opening with per-tunnel concurrency limits.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use axum::http::Uri;
use tokio::sync::{mpsc, oneshot, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::config::{host_without_port, ServerConfig, StaticRouteSettings, TunnelSettings};
use crate::error::{Error, Result};
use crate::httpfwd::{self, strip_mount};
use crate::metrics::{Metrics, TunnelStats};
use crate::upstream::{self, HttpClient};
use crate::util::unix_now;

/// How long the gateway waits for the agent to hand back a freshly opened stream.
pub const OPEN_STREAM_TIMEOUT: Duration = Duration::from_secs(10);

/// A request from the ingress to open a data stream towards an agent.
#[derive(Debug)]
pub struct OpenRequest {
    /// Channel used to return the stream or an error string.
    pub respond: oneshot::Sender<std::result::Result<yamux::Stream, String>>,
}

/// An established agent session.
#[derive(Debug)]
pub struct ActiveSession {
    /// Server generated session id.
    pub id: String,
    /// Tunnel id this session serves.
    pub tunnel_id: String,
    /// Agent build version.
    pub agent_version: String,
    /// Protocol families advertised by the agent.
    pub protocols: Vec<String>,
    /// Capability flags advertised by the agent.
    pub capabilities: Vec<String>,
    /// Agent peer address.
    pub peer: SocketAddr,
    /// When the session was established.
    pub opened_at: Instant,
    /// UNIX seconds of the last control/data activity (shared with the driver).
    pub last_seen_unix: Arc<AtomicU64>,
    /// Sender side of the outbound stream open queue.
    open_tx: mpsc::Sender<OpenRequest>,
    /// Cancellation token; cancelling tears down the session.
    pub cancel: CancellationToken,
    /// Maximum concurrent data streams negotiated with the agent.
    pub max_streams: u32,
}

impl ActiveSession {
    /// Create the request channel used to open data streams on a session.
    pub fn channels() -> (mpsc::Sender<OpenRequest>, mpsc::Receiver<OpenRequest>) {
        mpsc::channel(256)
    }

    /// Build a session handle from the pieces assembled during the handshake.
    ///
    /// `cancel`, `open_tx` and `last_seen` are created before the handshake so a
    /// driver task can keep the multiplexer polled while hello/hello_ack are
    /// exchanged (yamux only flushes frames while the connection is polled).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: String,
        tunnel_id: String,
        agent_version: String,
        protocols: Vec<String>,
        capabilities: Vec<String>,
        peer: SocketAddr,
        max_streams: u32,
        cancel: CancellationToken,
        open_tx: mpsc::Sender<OpenRequest>,
        last_seen: Arc<AtomicU64>,
    ) -> Arc<Self> {
        Arc::new(ActiveSession {
            id,
            tunnel_id,
            agent_version,
            protocols,
            capabilities,
            peer,
            opened_at: Instant::now(),
            last_seen_unix: last_seen,
            open_tx,
            cancel,
            max_streams,
        })
    }

    /// Update the last-seen timestamp.
    pub fn touch(&self) {
        self.last_seen_unix.store(unix_now(), Ordering::Relaxed);
    }

    /// Shared last-seen cell (used by the driver task).
    pub fn last_seen_handle(&self) -> Arc<AtomicU64> {
        self.last_seen_unix.clone()
    }

    /// Ask the agent to open a new data stream.
    pub async fn open_stream(&self) -> Result<yamux::Stream> {
        if self.cancel.is_cancelled() {
            return Err(Error::Unavailable("tunnel session has been closed".into()));
        }
        let (tx, rx) = oneshot::channel();
        self.open_tx
            .send(OpenRequest { respond: tx })
            .await
            .map_err(|_| Error::Unavailable("tunnel session is shutting down".into()))?;
        match tokio::time::timeout(OPEN_STREAM_TIMEOUT, rx).await {
            Err(_) => Err(Error::Timeout("timed out opening tunnel stream".into())),
            Ok(Err(_)) => Err(Error::Unavailable(
                "tunnel session closed before stream could be opened".into(),
            )),
            Ok(Ok(Ok(stream))) => Ok(stream),
            Ok(Ok(Err(message))) => Err(Error::Unavailable(message)),
        }
    }
}

/// A configured tunnel plus its live session, limits and stats.
#[derive(Debug)]
pub struct TunnelEntry {
    /// Static configuration.
    pub settings: TunnelSettings,
    /// Normalized mount prefix.
    pub mount: Option<String>,
    /// Normalized host patterns (lowercase).
    pub hosts: Vec<String>,
    /// Maximum concurrent data streams.
    pub max_streams: usize,
    /// Timeout waiting for upstream response headers.
    pub request_timeout: Duration,
    /// Maximum quiet period while streaming a response.
    pub stream_idle_timeout: Duration,
    /// Per-tunnel metrics.
    pub stats: Arc<TunnelStats>,
    /// Stream concurrency limiter.
    semaphore: Arc<Semaphore>,
    /// Live sessions. Multiple agents may serve the same tunnel for
    /// scale-out/failover; requests are distributed round-robin.
    sessions: RwLock<Vec<Arc<ActiveSession>>>,
    /// Round-robin cursor across `sessions`.
    cursor: AtomicUsize,
}

impl TunnelEntry {
    fn new(
        settings: TunnelSettings,
        metrics: &Metrics,
        default_max_streams: usize,
        default_request_timeout: Duration,
        default_stream_idle_timeout: Duration,
    ) -> Result<Self> {
        let mount = match &settings.mount {
            Some(m) => Some(crate::config::normalize_mount(m)?),
            None => None,
        };
        let hosts = settings
            .hosts
            .iter()
            .map(|h| crate::config::normalize_host(h))
            .collect::<Result<Vec<_>>>()?;
        let max_streams = settings.max_streams.unwrap_or(default_max_streams).max(1);
        let request_timeout = settings.request_timeout.unwrap_or(default_request_timeout);
        Ok(TunnelEntry {
            stats: metrics.tunnel(&settings.id),
            mount,
            hosts,
            max_streams,
            request_timeout,
            stream_idle_timeout: default_stream_idle_timeout,
            semaphore: Arc::new(Semaphore::new(max_streams)),
            sessions: RwLock::new(Vec::new()),
            cursor: AtomicUsize::new(0),
            settings,
        })
    }

    /// Snapshot of the live sessions.
    pub fn sessions(&self) -> Vec<Arc<ActiveSession>> {
        self.sessions
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Number of live sessions.
    pub fn session_count(&self) -> usize {
        self.sessions
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    /// The first live session (for status/diagnostics).
    pub fn session(&self) -> Option<Arc<ActiveSession>> {
        self.sessions().into_iter().next()
    }

    /// Whether at least one agent is currently connected.
    pub fn is_connected(&self) -> bool {
        self.sessions()
            .iter()
            .any(|session| !session.cancel.is_cancelled())
    }

    /// Register a session for this tunnel.
    ///
    /// Multiple agents may share one tunnel (scale-out / failover). Re-connecting
    /// with the same session id replaces that entry; other sessions are kept.
    pub fn install_session(
        self: &Arc<Self>,
        session: Arc<ActiveSession>,
    ) -> Option<Arc<ActiveSession>> {
        let mut guard = self.sessions.write().unwrap_or_else(|e| e.into_inner());
        let replaced = if let Some(slot) = guard.iter_mut().find(|s| s.id == session.id) {
            Some(std::mem::replace(slot, session))
        } else {
            guard.push(session);
            None
        };
        drop(guard);
        if let Some(old) = &replaced {
            // Only reachable when the same agent re-registers the same session id;
            // retire the stale connection.
            old.cancel.cancel();
        }
        self.stats.touch();
        replaced
    }

    /// Remove a session by id.
    pub fn remove_session(&self, session_id: &str) -> Option<Arc<ActiveSession>> {
        let mut guard = self.sessions.write().unwrap_or_else(|e| e.into_inner());
        let removed = guard
            .iter()
            .position(|s| s.id == session_id)
            .map(|index| guard.remove(index));
        drop(guard);
        removed
    }

    /// Drop sessions whose cancellation token fired.
    fn prune_sessions(&self) {
        let mut guard = self.sessions.write().unwrap_or_else(|e| e.into_inner());
        guard.retain(|session| !session.cancel.is_cancelled());
    }

    /// Open a data stream, respecting the per-tunnel concurrency limit.
    ///
    /// Sessions are tried round-robin; a session that fails to open a stream is
    /// cancelled (its agent is gone) and the next one is tried, which gives
    /// transparent failover when several agents serve the same tunnel.
    pub async fn open_stream(&self) -> Result<(yamux::Stream, OwnedSemaphorePermit)> {
        let permit = self.semaphore.clone().try_acquire_owned().map_err(|_| {
            Error::Limit(format!(
                "tunnel `{}` has {}/{} streams in flight",
                self.settings.id, self.max_streams, self.max_streams
            ))
        })?;
        self.prune_sessions();
        let sessions = self.sessions();
        if sessions.is_empty() {
            return Err(Error::Unavailable(format!(
                "tunnel `{}` is not connected",
                self.settings.id
            )));
        }
        let start = self.cursor.fetch_add(1, Ordering::Relaxed) % sessions.len();
        let mut last_error = None;
        for offset in 0..sessions.len() {
            let session = &sessions[(start + offset) % sessions.len()];
            if session.cancel.is_cancelled() {
                continue;
            }
            match session.open_stream().await {
                Ok(stream) => {
                    self.stats.touch();
                    return Ok((stream, permit));
                }
                Err(err) => {
                    // The agent behind this session is unreachable; retire it and
                    // fail over to the remaining sessions.
                    session.cancel.cancel();
                    last_error = Some(err);
                }
            }
        }
        self.prune_sessions();
        Err(last_error.unwrap_or_else(|| {
            Error::Unavailable(format!(
                "tunnel `{}` has no reachable agent",
                self.settings.id
            ))
        }))
    }
}

/// A direct reverse-proxy route (new-api, dashboards, ...).
#[derive(Debug)]
pub struct StaticRoute {
    /// Static configuration.
    pub settings: StaticRouteSettings,
    /// Normalized mount prefix.
    pub mount: String,
    /// Parsed upstream base URI.
    pub upstream: Uri,
    /// Pooled HTTP client configured with this route's CA bundle.
    pub client: HttpClient,
}

impl StaticRoute {
    /// Build the target URI for a forwarded path.
    pub fn target_uri(&self, path: &str, query: Option<&str>) -> Result<Uri> {
        httpfwd::join_upstream_uri(&self.upstream, path, query)
    }
}

/// Result of resolving a public request to a route.
#[derive(Debug, Clone)]
pub enum Resolved {
    /// Forward through a tunnel to an agent.
    Tunnel {
        /// Tunnel entry.
        tunnel: Arc<TunnelEntry>,
        /// Path with mount prefix stripped.
        path: String,
    },
    /// Forward directly to a static upstream.
    Static {
        /// Static route.
        route: Arc<StaticRoute>,
        /// Path with mount prefix stripped.
        path: String,
    },
}

/// The immutable routing table plus mutable session handles.
#[derive(Debug)]
pub struct Registry {
    tunnels: HashMap<String, Arc<TunnelEntry>>,
    static_routes: Vec<Arc<StaticRoute>>,
    default_tunnel: Option<Arc<TunnelEntry>>,
    metrics: Arc<Metrics>,
}

impl Registry {
    /// Build the registry from a validated config.
    pub fn new(config: &ServerConfig, metrics: Arc<Metrics>) -> Result<Arc<Self>> {
        let mut tunnels = HashMap::with_capacity(config.tunnels.len());
        let mut default_tunnel = None;
        for settings in &config.tunnels {
            if !settings.enabled {
                continue;
            }
            let entry = Arc::new(TunnelEntry::new(
                settings.clone(),
                &metrics,
                config.limits.max_streams_per_tunnel,
                config.limits.request_timeout,
                config.limits.stream_idle_timeout,
            )?);
            if settings.default {
                default_tunnel = Some(entry.clone());
            }
            tunnels.insert(settings.id.clone(), entry);
        }

        let mut static_routes = Vec::with_capacity(config.static_routes.len());
        for settings in &config.static_routes {
            let mount = crate::config::normalize_mount(&settings.mount)?;
            let upstream: Uri = settings.upstream.parse().map_err(|e| {
                Error::Config(format!("invalid upstream `{}`: {e}", settings.upstream))
            })?;
            let client =
                upstream::build_http_client(settings.tls_ca.as_deref(), Duration::from_secs(5))?;
            static_routes.push(Arc::new(StaticRoute {
                settings: settings.clone(),
                mount,
                upstream,
                client,
            }));
        }

        Ok(Arc::new(Registry {
            tunnels,
            static_routes,
            default_tunnel,
            metrics,
        }))
    }

    /// Metrics registry.
    pub fn metrics(&self) -> &Arc<Metrics> {
        &self.metrics
    }

    /// All configured tunnel entries.
    pub fn tunnels(&self) -> Vec<Arc<TunnelEntry>> {
        self.tunnels.values().cloned().collect()
    }

    /// Look up a tunnel by id.
    pub fn tunnel(&self, id: &str) -> Option<Arc<TunnelEntry>> {
        self.tunnels.get(id).cloned()
    }

    /// Whether any tunnel is configured.
    pub fn has_tunnels(&self) -> bool {
        !self.tunnels.is_empty()
    }

    /// Resolve a public request to a tunnel or static route.
    ///
    /// Precedence: host-matched tunnel (with optional mount) > longest mount
    /// prefix among tunnels/static routes > default tunnel.
    pub fn resolve(&self, host: Option<&str>, path: &str) -> Option<Resolved> {
        let host = host.map(host_without_port);
        let mut best: Option<(u32, Resolved)> = None;

        for entry in self.tunnels.values() {
            if !entry.settings.enabled {
                continue;
            }
            let host_match = match &host {
                Some(h) => !entry.hosts.is_empty() && entry.hosts.iter().any(|p| p == h),
                None => false,
            };
            if !entry.hosts.is_empty() && !host_match {
                continue;
            }
            let (stripped, mount_len) = match &entry.mount {
                Some(mount) => match strip_mount(path, mount) {
                    Some(stripped) => (stripped, mount.len() as u32),
                    None => continue,
                },
                None => (path.to_string(), 0),
            };
            let score = if host_match {
                10_000 + mount_len
            } else if entry.mount.is_some() {
                1_000 + mount_len
            } else if entry.settings.default {
                0
            } else {
                // Unreachable tunnel (validated at startup).
                continue;
            };
            if best.as_ref().map(|(s, _)| score > *s).unwrap_or(true) {
                best = Some((
                    score,
                    Resolved::Tunnel {
                        tunnel: entry.clone(),
                        path: stripped,
                    },
                ));
            }
        }

        for route in &self.static_routes {
            if let Some(stripped) = strip_mount(path, &route.mount) {
                let score = 1_000 + route.mount.len() as u32;
                if best.as_ref().map(|(s, _)| score > *s).unwrap_or(true) {
                    best = Some((
                        score,
                        Resolved::Static {
                            route: route.clone(),
                            path: stripped,
                        },
                    ));
                }
            }
        }

        if best.is_none() {
            if let Some(default) = &self.default_tunnel {
                // Mount-less default tunnel forwards the original path.
                return Some(Resolved::Tunnel {
                    tunnel: default.clone(),
                    path: path.to_string(),
                });
            }
        }
        best.map(|(_, r)| r)
    }

    /// Install an agent session. Additional sessions for the same tunnel join the
    /// rotation (multi-agent scale-out/failover); a session with the same id is
    /// replaced.
    pub fn install_session(
        &self,
        tunnel_id: &str,
        session: Arc<ActiveSession>,
    ) -> Result<Option<Arc<ActiveSession>>> {
        let entry = self
            .tunnels
            .get(tunnel_id)
            .ok_or_else(|| Error::Auth(format!("unknown or disabled tunnel `{tunnel_id}`")))?;
        let old = entry.install_session(session);
        Ok(old)
    }

    /// Remove one session of a tunnel.
    pub fn remove_session(&self, tunnel_id: &str, session_id: &str) {
        if let Some(entry) = self.tunnels.get(tunnel_id) {
            let removed = entry.remove_session(session_id);
            if removed.is_some() {
                entry.stats.touch();
            }
        }
    }

    /// JSON status document for `/status`.
    pub fn status_json(&self) -> serde_json::Value {
        let mut tunnels = Vec::with_capacity(self.tunnels.len());
        for entry in self.tunnels.values() {
            let sessions = entry.sessions();
            tunnels.push(serde_json::json!({
                "id": entry.settings.id,
                "mount": entry.mount,
                "hosts": entry.hosts,
                "connected": entry.is_connected(),
                "agents": sessions.len(),
                "max_streams": entry.max_streams,
                // `session` is kept for compatibility: the first live agent.
                "session": sessions.first().map(|s| serde_json::json!({
                    "id": s.id,
                    "agent_version": s.agent_version,
                    "peer": s.peer.to_string(),
                    "protocols": s.protocols,
                    "opened_seconds_ago": s.opened_at.elapsed().as_secs(),
                    "last_seen_unix": s.last_seen_unix.load(Ordering::Relaxed),
                    "max_streams": s.max_streams,
                })),
                "sessions": sessions.iter().map(|s| serde_json::json!({
                    "id": s.id,
                    "agent_version": s.agent_version,
                    "peer": s.peer.to_string(),
                    "protocols": s.protocols,
                    "opened_seconds_ago": s.opened_at.elapsed().as_secs(),
                    "last_seen_unix": s.last_seen_unix.load(Ordering::Relaxed),
                    "max_streams": s.max_streams,
                })).collect::<Vec<_>>(),
                "stats": {
                    "active_requests": entry.stats.active.load(Ordering::Relaxed),
                    "requests_total": entry.stats.requests.load(Ordering::Relaxed),
                    "errors_total": entry.stats.errors.load(Ordering::Relaxed),
                    "bytes_in_total": entry.stats.bytes_in.load(Ordering::Relaxed),
                    "bytes_out_total": entry.stats.bytes_out.load(Ordering::Relaxed),
                },
            }));
        }
        let static_routes: Vec<serde_json::Value> = self
            .static_routes
            .iter()
            .map(|r| {
                serde_json::json!({
                    "mount": r.mount,
                    "upstream": r.upstream.to_string(),
                    "strip_prefix": r.settings.strip_prefix,
                })
            })
            .collect();
        serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "uptime_seconds": self.metrics.uptime().as_secs(),
            "tunnels": tunnels,
            "static_routes": static_routes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TunnelSettings;

    fn tunnel(id: &str, mount: Option<&str>, hosts: &[&str], default: bool) -> TunnelSettings {
        TunnelSettings {
            id: id.to_string(),
            token: "token".into(),
            mount: mount.map(|m| m.to_string()),
            hosts: hosts.iter().map(|h| h.to_string()).collect(),
            default,
            protocols: vec![],
            upstream_api_key: None,
            upstream_auth_header: Default::default(),
            public_paths: vec![],
            max_streams: None,
            request_timeout: None,
            enabled: true,
        }
    }

    fn config() -> ServerConfig {
        let mut config = ServerConfig::default();
        config.api_keys.push(crate::config::ApiKeySettings {
            name: "k".into(),
            key: "key".into(),
            allow_tunnels: vec![],
            allow_ips: vec![],
            rate_limit: None,
            max_concurrent: 0,
        });
        config.tunnels = vec![
            tunnel("home", Some("/home"), &[], false),
            tunnel("anthropic", Some("/claude"), &[], false),
            tunnel("vhost", None, &["llm.example.com"], false),
            tunnel("fallback", None, &[], true),
        ];
        config.static_routes.push(StaticRouteSettings {
            mount: "/newapi".into(),
            upstream: "http://127.0.0.1:3000".into(),
            strip_prefix: true,
            require_auth: true,
            tls_ca: None,
            headers: Default::default(),
        });
        config
    }

    #[tokio::test]
    async fn resolves_mounts_hosts_and_default() {
        let config = config();
        let registry = Registry::new(&config, Arc::new(Metrics::new())).unwrap();
        match registry.resolve(None, "/home/v1/models") {
            Some(Resolved::Tunnel { tunnel, path }) => {
                assert_eq!(tunnel.settings.id, "home");
                assert_eq!(path, "/v1/models");
            }
            other => panic!("unexpected {other:?}"),
        }
        match registry.resolve(Some("llm.example.com"), "/v1/chat/completions") {
            Some(Resolved::Tunnel { tunnel, path }) => {
                assert_eq!(tunnel.settings.id, "vhost");
                assert_eq!(path, "/v1/chat/completions");
            }
            other => panic!("unexpected {other:?}"),
        }
        match registry.resolve(None, "/newapi/v1/models") {
            Some(Resolved::Static { route, path }) => {
                assert_eq!(route.mount, "/newapi");
                assert_eq!(path, "/v1/models");
            }
            other => panic!("unexpected {other:?}"),
        }
        match registry.resolve(None, "/unmatched") {
            Some(Resolved::Tunnel { tunnel, .. }) => assert_eq!(tunnel.settings.id, "fallback"),
            other => panic!("unexpected {other:?}"),
        }
        // Host match wins over mount match.
        match registry.resolve(Some("llm.example.com:443"), "/home/v1/models") {
            Some(Resolved::Tunnel { tunnel, path }) => {
                assert_eq!(tunnel.settings.id, "vhost");
                assert_eq!(path, "/home/v1/models");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn installs_and_replaces_sessions() {
        let config = config();
        let registry = Registry::new(&config, Arc::new(Metrics::new())).unwrap();
        let (tx1, _rx1) = ActiveSession::channels();
        let session1 = ActiveSession::new(
            "s1".into(),
            "home".into(),
            "agent".into(),
            vec![],
            vec![],
            "127.0.0.1:5555".parse().unwrap(),
            8,
            CancellationToken::new(),
            tx1,
            Arc::new(AtomicU64::new(unix_now())),
        );
        registry.install_session("home", session1.clone()).unwrap();
        let entry = registry.tunnel("home").unwrap();
        assert!(entry.is_connected());
        assert_eq!(entry.session_count(), 1);
        // A second agent joins the same tunnel (multi-agent scale-out).
        let (tx2, _rx2) = ActiveSession::channels();
        let session2 = ActiveSession::new(
            "s2".into(),
            "home".into(),
            "agent".into(),
            vec![],
            vec![],
            "127.0.0.1:5556".parse().unwrap(),
            8,
            CancellationToken::new(),
            tx2,
            Arc::new(AtomicU64::new(unix_now())),
        );
        assert!(registry
            .install_session("home", session2.clone())
            .unwrap()
            .is_none());
        assert_eq!(entry.session_count(), 2);
        assert!(!session1.cancel.is_cancelled());
        // Re-registering the same session id replaces (and cancels) the old one.
        let (tx3, _rx3) = ActiveSession::channels();
        let session1b = ActiveSession::new(
            "s1".into(),
            "home".into(),
            "agent".into(),
            vec![],
            vec![],
            "127.0.0.1:5557".parse().unwrap(),
            8,
            CancellationToken::new(),
            tx3,
            Arc::new(AtomicU64::new(unix_now())),
        );
        let replaced = registry.install_session("home", session1b).unwrap();
        assert!(replaced.is_some());
        assert!(session1.cancel.is_cancelled());
        assert_eq!(entry.session_count(), 2);
        registry.remove_session("home", "s2");
        assert_eq!(entry.session_count(), 1);
        registry.remove_session("home", "s1");
        assert!(!entry.is_connected());
        assert_eq!(entry.session_count(), 0);
    }

    #[tokio::test]
    async fn rejects_unknown_tunnel_session() {
        let config = config();
        let registry = Registry::new(&config, Arc::new(Metrics::new())).unwrap();
        let (tx, _rx) = ActiveSession::channels();
        let session = ActiveSession::new(
            "s".into(),
            "nope".into(),
            "agent".into(),
            vec![],
            vec![],
            "127.0.0.1:1".parse().unwrap(),
            1,
            CancellationToken::new(),
            tx,
            Arc::new(AtomicU64::new(unix_now())),
        );
        assert!(registry.install_session("nope", session).is_err());
    }

    #[tokio::test]
    async fn opening_without_session_fails() {
        let config = config();
        let registry = Registry::new(&config, Arc::new(Metrics::new())).unwrap();
        let entry = registry.tunnel("home").unwrap();
        let err = entry.open_stream().await.unwrap_err();
        assert!(matches!(err, Error::Unavailable(_)), "{err}");
    }

    #[test]
    fn status_contains_tunnel() {
        let config = config();
        let registry = Registry::new(&config, Arc::new(Metrics::new())).unwrap();
        let status = registry.status_json();
        assert!(status["tunnels"].as_array().unwrap().len() >= 4);
        assert!(status["version"].is_string());
    }
}
