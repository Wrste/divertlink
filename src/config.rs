//! Configuration model for both ends of the tunnel.
//!
//! Every config type has a hand-written [`Default`] implementation so that
//! `deny_unknown_fields` catches typos while minimal configs stay short. Relative
//! certificate paths are resolved against the directory containing the config
//! file, which makes `divertlink server -c deploy/server.toml` work from anywhere.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::protocol::TunnelProtocol;
use crate::ratelimit::RateSpec;
use crate::util::ByteSize;

/// Byte size defaults (64 MiB request bodies cover base64 images and tool payloads).
pub const DEFAULT_MAX_REQUEST_BYTES: ByteSize = ByteSize(64 * 1024 * 1024);

/// Default application heartbeat interval.
pub const DEFAULT_HEARTBEAT: Duration = Duration::from_secs(5);
/// Default application heartbeat timeout.
pub const DEFAULT_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(15);

fn d_bytes() -> ByteSize {
    DEFAULT_MAX_REQUEST_BYTES
}
fn d_10m() -> Duration {
    Duration::from_secs(600)
}
fn d_120s() -> Duration {
    Duration::from_secs(120)
}
fn d_10s() -> Duration {
    Duration::from_secs(10)
}
fn d_5s() -> Duration {
    DEFAULT_HEARTBEAT
}
fn d_15s() -> Duration {
    DEFAULT_HEARTBEAT_TIMEOUT
}
fn d_1s() -> Duration {
    Duration::from_secs(1)
}
fn d_60s() -> Duration {
    Duration::from_secs(60)
}
fn d_256() -> usize {
    256
}
fn d_1024() -> usize {
    1024
}
fn d_128() -> usize {
    128
}
fn d_true() -> bool {
    true
}
fn d_0_3() -> f64 {
    0.3
}
fn d_cert() -> PathBuf {
    PathBuf::from("certs/server.crt")
}
fn d_key() -> PathBuf {
    PathBuf::from("certs/server.key")
}

/// TLS protocol version floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TlsVersion {
    /// TLS 1.2 (for legacy clients / old OpenSSL).
    #[serde(rename = "1.2")]
    V1_2,
    /// TLS 1.3 only.
    #[serde(rename = "1.3")]
    V1_3,
}

// TLS 1.3 is the default floor. Written explicitly (not derived) to document
// the security default at the definition site.
#[allow(clippy::derivable_impls)]
impl Default for TlsVersion {
    fn default() -> Self {
        TlsVersion::V1_3
    }
}

impl TlsVersion {
    /// Rustls protocol versions for this floor.
    pub fn protocol_versions(&self) -> &'static [&'static rustls::SupportedProtocolVersion] {
        static TLS12_AND_13: &[&rustls::SupportedProtocolVersion] =
            &[&rustls::version::TLS13, &rustls::version::TLS12];
        static TLS13_ONLY: &[&rustls::SupportedProtocolVersion] = &[&rustls::version::TLS13];
        match self {
            TlsVersion::V1_2 => TLS12_AND_13,
            TlsVersion::V1_3 => TLS13_ONLY,
        }
    }
}

/// Heartbeat mechanism used by the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum HeartbeatMode {
    /// Application-level JSON `ping`/`pong` on the control stream (default).
    #[default]
    App,
    /// Rely on TCP keepalive only.
    Transport,
    /// Both application and transport keepalives.
    Both,
    /// Disable all keepalives (not recommended).
    None,
}

impl HeartbeatMode {
    /// Whether application-level ping/pong is enabled.
    pub fn app_enabled(&self) -> bool {
        matches!(self, HeartbeatMode::App | HeartbeatMode::Both)
    }

    /// Whether transport-level (TCP) keepalive is enabled.
    pub fn transport_enabled(&self) -> bool {
        matches!(self, HeartbeatMode::Transport | HeartbeatMode::Both)
    }
}

/// How the gateway replaces the inbound authentication header before forwarding
/// requests to the model server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum UpstreamAuth {
    /// Forward the client's `Authorization` / `x-api-key` untouched (default).
    #[default]
    #[serde(rename = "passthrough")]
    Passthrough,
    /// Replace `Authorization: Bearer <upstream_api_key>`.
    #[serde(rename = "authorization")]
    Authorization,
    /// Replace `x-api-key: <upstream_api_key>`.
    #[serde(rename = "x-api-key")]
    XApiKey,
    /// Strip authentication headers entirely.
    #[serde(rename = "none")]
    None,
}

/// Log output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    /// Human readable text.
    #[default]
    Text,
    /// Newline delimited JSON, suitable for log collectors.
    Json,
}

/// Log configuration shared by both ends.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LogSettings {
    /// `tracing` filter directive, e.g. `info` or `divertlink=debug,tower=warn`.
    pub level: String,
    /// Output format.
    pub format: LogFormat,
}

impl Default for LogSettings {
    fn default() -> Self {
        LogSettings {
            level: "info".to_string(),
            format: LogFormat::Text,
        }
    }
}

/// Deserialize a listener address. `"off"`, `"none"`, `"disabled"`, `""` or
/// `false` disable the listener; any other string must be a `host:port` address.
fn deserialize_listen<'de, D>(deserializer: D) -> std::result::Result<Option<SocketAddr>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{Error as DeError, Visitor};

    struct ListenVisitor;

    impl Visitor<'_> for ListenVisitor {
        type Value = Option<SocketAddr>;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(
                "a socket address such as \"0.0.0.0:443\", or \"off\"/false to disable the listener",
            )
        }

        fn visit_bool<E: DeError>(self, value: bool) -> std::result::Result<Self::Value, E> {
            if value {
                Err(E::custom(
                    "a listener must be an address string; use \"off\" or false to disable",
                ))
            } else {
                Ok(None)
            }
        }

        fn visit_str<E: DeError>(self, value: &str) -> std::result::Result<Self::Value, E> {
            let value = value.trim();
            if value.is_empty()
                || value.eq_ignore_ascii_case("off")
                || value.eq_ignore_ascii_case("none")
                || value.eq_ignore_ascii_case("disabled")
            {
                return Ok(None);
            }
            value.parse::<SocketAddr>().map(Some).map_err(|e| {
                E::custom(format!(
                    "invalid socket address `{value}`: {e} (use \"host:port\" or \"off\")"
                ))
            })
        }
    }

    deserializer.deserialize_any(ListenVisitor)
}

/// Gateway listener and identity settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ServerSettings {
    /// Human readable gateway name, shown in status output.
    pub name: String,
    /// Agent-facing TLS tunnel listener. Set to `"off"` to disable tunnel acceptance.
    #[serde(default = "d_tunnel_listen", deserialize_with = "deserialize_listen")]
    pub tunnel_listen: Option<SocketAddr>,
    /// Public TLS ingress listener. May equal `tunnel_listen` to share :443 via ALPN.
    #[serde(default = "d_ingress_listen", deserialize_with = "deserialize_listen")]
    pub ingress_listen: Option<SocketAddr>,
    /// Optional plaintext ingress listener (put it behind nginx/Caddy or on localhost).
    /// Set to `"off"` to disable it.
    #[serde(default = "d_http_listen", deserialize_with = "deserialize_listen")]
    pub http_listen: Option<SocketAddr>,
    /// Admin endpoints (`/healthz`, `/readyz`, `/metrics`, `/status`). Keep on localhost.
    /// Set to `"off"` to disable them.
    #[serde(default = "d_admin_listen", deserialize_with = "deserialize_listen")]
    pub admin_listen: Option<SocketAddr>,
    /// Trust `X-Forwarded-For` for the client IP (enable only behind a trusted proxy).
    pub trust_proxy: bool,
    /// How long in-flight requests and SSE streams may drain on shutdown.
    #[serde(default = "d_10s", with = "humantime_serde")]
    pub shutdown_grace: Duration,
}

fn d_tunnel_listen() -> Option<SocketAddr> {
    Some(SocketAddr::from(([0, 0, 0, 0], 443)))
}

fn d_ingress_listen() -> Option<SocketAddr> {
    Some(SocketAddr::from(([0, 0, 0, 0], 443)))
}

fn d_http_listen() -> Option<SocketAddr> {
    Some(SocketAddr::from(([127, 0, 0, 1], 8080)))
}

fn d_admin_listen() -> Option<SocketAddr> {
    Some(SocketAddr::from(([127, 0, 0, 1], 9090)))
}

impl Default for ServerSettings {
    fn default() -> Self {
        ServerSettings {
            name: "divertlink-gateway".to_string(),
            tunnel_listen: Some(SocketAddr::from(([0, 0, 0, 0], 443))),
            ingress_listen: Some(SocketAddr::from(([0, 0, 0, 0], 443))),
            http_listen: Some(SocketAddr::from(([127, 0, 0, 1], 8080))),
            admin_listen: Some(SocketAddr::from(([127, 0, 0, 1], 9090))),
            trust_proxy: false,
            shutdown_grace: d_10s(),
        }
    }
}

/// Server side TLS material.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TlsServerSettings {
    /// PEM certificate chain.
    #[serde(default = "d_cert")]
    pub cert: PathBuf,
    /// PEM private key (PKCS#8, PKCS#1 or SEC1).
    #[serde(default = "d_key")]
    pub key: PathBuf,
    /// Optional client CA bundle: enables mutual TLS for agent connections.
    /// Requires a dedicated `tunnel_listen` separate from `ingress_listen`.
    pub client_ca: Option<PathBuf>,
    /// Minimum accepted TLS version.
    pub min_version: TlsVersion,
}

impl Default for TlsServerSettings {
    fn default() -> Self {
        TlsServerSettings {
            cert: d_cert(),
            key: d_key(),
            client_ca: None,
            min_version: TlsVersion::V1_3,
        }
    }
}

/// Resource limits enforced by the gateway.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LimitsSettings {
    /// Maximum request body size accepted from clients.
    #[serde(default = "d_bytes")]
    pub max_request_bytes: ByteSize,
    /// Maximum time to wait for upstream response headers.
    #[serde(default = "d_10m", with = "humantime_serde")]
    pub request_timeout: Duration,
    /// Maximum quiet period while streaming a response body.
    #[serde(default = "d_120s", with = "humantime_serde")]
    pub stream_idle_timeout: Duration,
    /// Maximum time to read client request headers.
    #[serde(default = "d_10s", with = "humantime_serde")]
    pub header_read_timeout: Duration,
    /// Default concurrent data streams per tunnel (can be overridden per tunnel).
    #[serde(default = "d_256")]
    pub max_streams_per_tunnel: usize,
    /// Maximum concurrent accepted tunnel connections.
    #[serde(default = "d_1024")]
    pub max_connections: usize,
    /// HTTP methods accepted by the ingress (others get 405).
    #[serde(default = "d_methods")]
    pub allowed_methods: Vec<String>,
    /// Optional gateway-wide rate limit (e.g. `"3000/min"`) applied before auth.
    #[serde(default)]
    pub global_rate_limit: Option<String>,
}

fn d_methods() -> Vec<String> {
    ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"]
        .into_iter()
        .map(str::to_string)
        .collect()
}

impl Default for LimitsSettings {
    fn default() -> Self {
        LimitsSettings {
            max_request_bytes: d_bytes(),
            request_timeout: d_10m(),
            stream_idle_timeout: d_120s(),
            header_read_timeout: d_10s(),
            max_streams_per_tunnel: d_256(),
            max_connections: d_1024(),
            allowed_methods: d_methods(),
            global_rate_limit: None,
        }
    }
}

/// Public API key accepted by the gateway ingress.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiKeySettings {
    /// Key name used in audit logs (never logs the secret itself).
    pub name: String,
    /// Secret presented by clients: `Authorization: Bearer <key>` or `x-api-key: <key>`.
    pub key: String,
    /// Tunnel ids this key may access. Empty means all tunnels.
    #[serde(default)]
    pub allow_tunnels: Vec<String>,
    /// Client IP allow list (CIDR). Empty means any source.
    #[serde(default)]
    pub allow_ips: Vec<String>,
    /// Token bucket limit, e.g. `600/min`. Empty/None means unlimited.
    #[serde(default)]
    pub rate_limit: Option<String>,
    /// Maximum concurrent requests across this key. `0` means unlimited.
    #[serde(default)]
    pub max_concurrent: usize,
}

/// A statically configured tunnel (one agent).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TunnelSettings {
    /// Tunnel id, must match the agent's `[agent] id`.
    pub id: String,
    /// Shared agent token (also configured on the agent).
    pub token: String,
    /// Ingress path prefix, e.g. `/home`. Stripped before forwarding.
    #[serde(default)]
    pub mount: Option<String>,
    /// Optional Host header routing (e.g. `home.llm.example.com`).
    #[serde(default)]
    pub hosts: Vec<String>,
    /// Fallback tunnel for unmatched requests. At most one default tunnel.
    #[serde(default)]
    pub default: bool,
    /// Advertised protocol families, for validation/metrics. Empty means all.
    #[serde(default)]
    pub protocols: Vec<String>,
    /// Replace inbound auth with this upstream key (see `upstream_auth_header`).
    #[serde(default)]
    pub upstream_api_key: Option<String>,
    /// How to treat inbound auth headers before forwarding.
    #[serde(default)]
    pub upstream_auth_header: UpstreamAuth,
    /// Path prefixes (relative to the tunnel mount) that skip API key auth, e.g. `["/health"]`.
    #[serde(default)]
    pub public_paths: Vec<String>,
    /// Concurrent stream limit for this tunnel (falls back to `[limits]`).
    #[serde(default)]
    pub max_streams: Option<usize>,
    /// Response header timeout for this tunnel (falls back to `[limits]`).
    #[serde(default, with = "humantime_serde::option")]
    pub request_timeout: Option<Duration>,
    /// Whether the tunnel is enabled.
    #[serde(default = "d_true")]
    pub enabled: bool,
}

impl TunnelSettings {
    /// The effective upstream API key (empty string means passthrough).
    pub fn upstream_key(&self) -> Option<&str> {
        self.upstream_api_key
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    /// Normalized mount prefix, if any.
    pub fn normalized_mount(&self) -> Option<String> {
        self.mount.as_deref().and_then(|m| normalize_mount(m).ok())
    }

    /// Whether this tunnel advertises a protocol family (empty = all).
    pub fn supports_protocol(&self, proto: TunnelProtocol) -> bool {
        self.protocols.is_empty()
            || self
                .protocols
                .iter()
                .any(|p| TunnelProtocol::from_name(p) == Some(proto))
    }
}

/// A direct reverse-proxy route (e.g. new-api on the same host).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StaticRouteSettings {
    /// Ingress path prefix.
    pub mount: String,
    /// Upstream base URL, e.g. `http://127.0.0.1:3000`.
    pub upstream: String,
    /// Strip the mount prefix before forwarding.
    #[serde(default = "d_true")]
    pub strip_prefix: bool,
    /// Require a gateway API key for this route (default true). Disable only when
    /// the upstream (for example new-api) performs its own authentication.
    #[serde(default = "d_true")]
    pub require_auth: bool,
    /// Extra CA bundle used to verify an HTTPS upstream.
    #[serde(default)]
    pub tls_ca: Option<PathBuf>,
    /// Additional headers added to upstream requests.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

/// The complete gateway configuration (`server.toml`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ServerConfig {
    /// Listeners and identity.
    pub server: ServerSettings,
    /// TLS material.
    pub tls: TlsServerSettings,
    /// Limits.
    pub limits: LimitsSettings,
    /// Logging.
    pub log: LogSettings,
    /// Public API keys.
    #[serde(default)]
    pub api_keys: Vec<ApiKeySettings>,
    /// Statically configured tunnels.
    #[serde(default)]
    pub tunnels: Vec<TunnelSettings>,
    /// Direct reverse-proxy routes.
    #[serde(default)]
    pub static_routes: Vec<StaticRouteSettings>,
}

#[allow(clippy::derivable_impls)]
impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            server: ServerSettings::default(),
            tls: TlsServerSettings::default(),
            limits: LimitsSettings::default(),
            log: LogSettings::default(),
            api_keys: Vec::new(),
            tunnels: Vec::new(),
            static_routes: Vec::new(),
        }
    }
}

impl ServerConfig {
    /// Load and validate a gateway config file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::Config(format!("cannot read {}: {e}", path.display())))?;
        let mut cfg: ServerConfig =
            toml::from_str(&text).map_err(|e| Error::Config(format!("{}: {e}", path.display())))?;
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        cfg.resolve_paths(base);
        cfg.validate()?;
        Ok(cfg)
    }

    /// Parse and validate a config from a TOML string (used by `divertlink check`).
    pub fn from_toml_str(text: &str) -> Result<Self> {
        let cfg: ServerConfig = toml::from_str(text)?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Resolve relative certificate paths against `base`.
    pub fn resolve_paths(&mut self, base: &Path) {
        resolve_path(base, &mut self.tls.cert);
        resolve_path(base, &mut self.tls.key);
        if let Some(p) = &mut self.tls.client_ca {
            resolve_path(base, p);
        }
        for route in &mut self.static_routes {
            if let Some(p) = &mut route.tls_ca {
                resolve_path(base, p);
            }
        }
    }

    /// Validate cross-field constraints with actionable error messages.
    pub fn validate(&self) -> Result<()> {
        if self.server.name.trim().is_empty() {
            return Err(Error::Config("server.name must not be empty".into()));
        }
        // Listeners: at least one ingress option.
        if self.server.tunnel_listen.is_none()
            && self.server.ingress_listen.is_none()
            && self.server.http_listen.is_none()
        {
            return Err(Error::Config(
                "at least one of server.tunnel_listen / ingress_listen / http_listen must be set"
                    .into(),
            ));
        }
        if let Some(t) = self.server.tunnel_listen {
            if let Some(a) = self.server.admin_listen {
                if same_listen(t, a) {
                    return Err(Error::Config(
                        "server.admin_listen must not collide with tunnel_listen".into(),
                    ));
                }
            }
            if let Some(h) = self.server.http_listen {
                if same_listen(t, h) {
                    return Err(Error::Config(
                        "server.http_listen must not collide with tunnel_listen".into(),
                    ));
                }
            }
        }
        if let (Some(i), Some(a)) = (self.server.ingress_listen, self.server.admin_listen) {
            if same_listen(i, a) {
                return Err(Error::Config(
                    "server.admin_listen must not collide with ingress_listen".into(),
                ));
            }
        }
        if let (Some(h), Some(a)) = (self.server.http_listen, self.server.admin_listen) {
            if same_listen(h, a) {
                return Err(Error::Config(
                    "server.admin_listen must not collide with http_listen".into(),
                ));
            }
        }
        if let (Some(t), Some(h)) = (self.server.tunnel_listen, self.server.http_listen) {
            if same_listen(t, h) {
                return Err(Error::Config(
                    "plaintext http_listen cannot serve the TLS tunnel port".into(),
                ));
            }
        }
        let tls_listener =
            self.server.tunnel_listen.is_some() || self.server.ingress_listen.is_some();
        if tls_listener
            && (self.tls.cert.as_os_str().is_empty() || self.tls.key.as_os_str().is_empty())
        {
            return Err(Error::Config(
                "tls.cert and tls.key are required when tunnel_listen or ingress_listen is enabled"
                    .into(),
            ));
        }
        if !tls_listener && self.tls.client_ca.is_some() {
            return Err(Error::Config(
                "tls.client_ca requires tunnel_listen to be enabled".into(),
            ));
        }
        if let (Some(i), Some(h)) = (self.server.ingress_listen, self.server.http_listen) {
            if same_listen(i, h) {
                return Err(Error::Config(
                    "server.http_listen must not collide with ingress_listen".into(),
                ));
            }
        }
        if self.tls.client_ca.is_some() {
            // mTLS on a shared ALPN port would also require client certs from browsers.
            let shared = match (self.server.tunnel_listen, self.server.ingress_listen) {
                (Some(t), Some(i)) => t == i,
                _ => false,
            };
            if shared {
                return Err(Error::Config(
                    "tls.client_ca (mTLS) requires a dedicated tunnel_listen that differs from ingress_listen"
                        .into(),
                ));
            }
            if self.server.tunnel_listen.is_none() {
                return Err(Error::Config(
                    "tls.client_ca requires server.tunnel_listen".into(),
                ));
            }
        }

        // Limits.
        if self.limits.max_request_bytes.bytes() == 0 {
            return Err(Error::Config("limits.max_request_bytes must be > 0".into()));
        }
        if self.limits.max_streams_per_tunnel == 0 {
            return Err(Error::Config(
                "limits.max_streams_per_tunnel must be > 0".into(),
            ));
        }
        if self.limits.request_timeout.is_zero() {
            return Err(Error::Config("limits.request_timeout must be > 0".into()));
        }
        if self.limits.stream_idle_timeout.is_zero() {
            return Err(Error::Config(
                "limits.stream_idle_timeout must be > 0".into(),
            ));
        }
        if self.limits.header_read_timeout.is_zero() {
            return Err(Error::Config(
                "limits.header_read_timeout must be > 0".into(),
            ));
        }

        // API keys.
        let mut key_names = std::collections::HashSet::new();
        let mut key_values = std::collections::HashSet::new();
        for (idx, k) in self.api_keys.iter().enumerate() {
            if k.name.trim().is_empty() {
                return Err(Error::Config(format!(
                    "api_keys[{idx}].name must not be empty"
                )));
            }
            if !key_names.insert(k.name.clone()) {
                return Err(Error::Config(format!(
                    "duplicate api key name `{}`",
                    k.name
                )));
            }
            if k.key.trim().is_empty() {
                return Err(Error::Config(format!(
                    "api_keys[{}].key must not be empty",
                    k.name
                )));
            }
            if !key_values.insert(k.key.clone()) {
                return Err(Error::Config(format!(
                    "duplicate api key secret for `{}`",
                    k.name
                )));
            }
            if k.key.chars().any(|c| c.is_whitespace()) {
                return Err(Error::Config(format!(
                    "api key `{}` must not contain whitespace",
                    k.name
                )));
            }
            if let Some(rate) = &k.rate_limit {
                rate.parse::<RateSpec>()
                    .map_err(|e| Error::Config(format!("api key `{}`: {e}", k.name)))?;
            }
            for cidr in &k.allow_ips {
                cidr.parse::<ipnet::IpNet>().map_err(|e| {
                    Error::Config(format!("api key `{}` allow_ips: `{cidr}`: {e}", k.name))
                })?;
            }
        }

        // Tunnels.
        let mut ids = std::collections::HashSet::new();
        let mut mounts = std::collections::HashSet::new();
        let mut hosts = std::collections::HashSet::new();
        let mut default_count = 0;
        for (idx, t) in self.tunnels.iter().enumerate() {
            if t.id.trim().is_empty() {
                return Err(Error::Config(format!(
                    "tunnels[{idx}].id must not be empty"
                )));
            }
            if t.id
                .chars()
                .any(|c| c.is_whitespace() || c == '/' || c == ':')
            {
                return Err(Error::Config(format!(
                    "tunnel id `{}` may not contain whitespace, '/' or ':'",
                    t.id
                )));
            }
            if !ids.insert(t.id.clone()) {
                return Err(Error::Config(format!("duplicate tunnel id `{}`", t.id)));
            }
            if t.token.trim().is_empty() {
                return Err(Error::Config(format!(
                    "tunnel `{}` token must not be empty",
                    t.id
                )));
            }
            if let Some(m) = &t.mount {
                let m = normalize_mount(m)?;
                if !mounts.insert(m.clone()) {
                    return Err(Error::Config(format!("duplicate mount `{m}`")));
                }
                if t.default {
                    return Err(Error::Config(format!(
                        "tunnel `{}` cannot set both mount and default=true",
                        t.id
                    )));
                }
            }
            if t.default {
                default_count += 1;
            }
            if t.mount.is_none() && t.hosts.is_empty() && !t.default {
                return Err(Error::Config(format!(
                    "tunnel `{}` is unreachable: set mount, hosts or default = true",
                    t.id
                )));
            }
            for h in &t.hosts {
                let h = normalize_host(h)?;
                if !hosts.insert(h.clone()) {
                    return Err(Error::Config(format!("duplicate host `{h}`")));
                }
            }
            for p in &t.protocols {
                if TunnelProtocol::from_name(p).is_none() {
                    return Err(Error::Config(format!(
                        "tunnel `{}` has unknown protocol `{p}` (use openai/anthropic/vllm/custom)",
                        t.id
                    )));
                }
            }
            for p in &t.public_paths {
                if !p.starts_with('/') {
                    return Err(Error::Config(format!(
                        "tunnel `{}` public_paths must start with '/': `{p}`",
                        t.id
                    )));
                }
            }
            if let Some(ms) = t.max_streams {
                if ms == 0 {
                    return Err(Error::Config(format!(
                        "tunnel `{}` max_streams must be > 0",
                        t.id
                    )));
                }
            }
        }
        if default_count > 1 {
            return Err(Error::Config(
                "at most one tunnel may set default = true".into(),
            ));
        }
        for key in &self.api_keys {
            for allowed in &key.allow_tunnels {
                if !ids.contains(allowed) {
                    return Err(Error::Config(format!(
                        "api key `{}` references unknown tunnel `{allowed}`",
                        key.name
                    )));
                }
            }
        }
        if self.server.tunnel_listen.is_some() && self.tunnels.is_empty() {
            return Err(Error::Config(
                "server.tunnel_listen is enabled but no [[tunnels]] are configured".into(),
            ));
        }
        let auth_required =
            !self.tunnels.is_empty() || self.static_routes.iter().any(|r| r.require_auth);
        if auth_required && self.api_keys.is_empty() {
            return Err(Error::Config(
                "at least one [[api_keys]] entry is required when tunnels or authenticated \
                 static routes are configured; the public ingress must never be unauthenticated \
                 (set require_auth = false on a static route only if the upstream authenticates)"
                    .into(),
            ));
        }
        if self.tunnels.is_empty() && self.static_routes.is_empty() {
            return Err(Error::Config(
                "configure at least one [[tunnels]] or [[static_routes]] route".into(),
            ));
        }

        // Static routes.
        for (idx, r) in self.static_routes.iter().enumerate() {
            let m = normalize_mount(&r.mount)?;
            if !mounts.insert(m.clone()) {
                return Err(Error::Config(format!(
                    "mount `{m}` is used by both a tunnel and static_routes[{idx}]"
                )));
            }
            if r.upstream.trim().is_empty() {
                return Err(Error::Config(format!(
                    "static_routes[{idx}].upstream must not be empty"
                )));
            }
            let uri: http::Uri = r
                .upstream
                .parse()
                .map_err(|e| Error::Config(format!("static_routes[{idx}].upstream: {e}")))?;
            if uri.scheme().is_none() || uri.authority().is_none() {
                return Err(Error::Config(format!(
                    "static_routes[{idx}].upstream must be an absolute URL with scheme and host"
                )));
            }
            if uri.scheme_str() != Some("http") && uri.scheme_str() != Some("https") {
                return Err(Error::Config(format!(
                    "static_routes[{idx}].upstream must use http:// or https://"
                )));
            }
        }

        Ok(())
    }

    /// A runnable, fully documented example configuration.
    pub fn example() -> &'static str {
        include_str!("../assets/server.example.toml")
    }

    /// The first tunnel with a given id.
    pub fn tunnel(&self, id: &str) -> Option<&TunnelSettings> {
        self.tunnels.iter().find(|t| t.id == id)
    }
}

/// Agent connection, identity and reconnect settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AgentSettings {
    /// Tunnel id, must match a `[[tunnels]]` entry on the gateway.
    pub id: String,
    /// Gateway endpoint, e.g. `gateway.example.com:443`, `tls://host:443` or `https://host:443`.
    pub server: String,
    /// Shared agent token.
    pub token: String,
    /// Application heartbeat interval.
    #[serde(default = "d_5s", with = "humantime_serde")]
    pub heartbeat: Duration,
    /// Application heartbeat timeout.
    #[serde(default = "d_15s", with = "humantime_serde")]
    pub heartbeat_timeout: Duration,
    /// Heartbeat mechanism.
    pub heartbeat_mode: HeartbeatMode,
    /// TCP connect + TLS handshake timeout.
    #[serde(default = "d_10s", with = "humantime_serde")]
    pub connect_timeout: Duration,
    /// Maximum concurrent in-flight requests from the gateway.
    #[serde(default = "d_128")]
    pub max_inflight: usize,
    /// Initial reconnect delay.
    #[serde(default = "d_1s", with = "humantime_serde")]
    pub reconnect_min: Duration,
    /// Maximum reconnect delay.
    #[serde(default = "d_60s", with = "humantime_serde")]
    pub reconnect_max: Duration,
    /// Jitter fraction applied to reconnect delays (0.0 - 1.0).
    #[serde(default = "d_0_3")]
    pub reconnect_jitter: f64,
    /// What the agent advertises to the gateway (`openai`, `anthropic`, `vllm`, ...).
    #[serde(default = "d_protocols")]
    pub protocols: Vec<String>,
}

fn d_protocols() -> Vec<String> {
    vec![
        "openai".to_string(),
        "anthropic".to_string(),
        "vllm".to_string(),
    ]
}

impl Default for AgentSettings {
    fn default() -> Self {
        AgentSettings {
            id: String::new(),
            server: String::new(),
            token: String::new(),
            heartbeat: d_5s(),
            heartbeat_timeout: d_15s(),
            heartbeat_mode: HeartbeatMode::App,
            connect_timeout: d_10s(),
            max_inflight: d_128(),
            reconnect_min: d_1s(),
            reconnect_max: d_60s(),
            reconnect_jitter: d_0_3(),
            protocols: d_protocols(),
        }
    }
}

impl AgentSettings {
    /// Parse `server` into `(host, port, servername)`.
    ///
    /// Accepted forms: `host:port`, `tls://host:port`, `https://host:port`.
    pub fn server_parts(&self) -> Result<(String, u16, String)> {
        let raw = self.server.trim();
        if raw.is_empty() {
            return Err(Error::Config("agent.server must not be empty".into()));
        }
        let (host_port, servername) = if let Some(rest) = raw
            .strip_prefix("tls://")
            .or_else(|| raw.strip_prefix("https://"))
        {
            let (host, port) = split_host_port(rest)?;
            (format!("{host}:{port}"), host)
        } else if raw.starts_with("http://") {
            return Err(Error::Config(
                "agent.server must use TLS (host:port, tls:// or https://); \
                 plain http:// is not supported for tunnels"
                    .into(),
            ));
        } else if raw.starts_with("wss://") || raw.starts_with("ws://") {
            return Err(Error::Config(
                "WebSocket transport is not supported; use tls://host:port".into(),
            ));
        } else {
            let (host, port) = split_host_port(raw)?;
            (format!("{host}:{port}"), host)
        };
        let port = port_of(&host_port)?;
        Ok((host_port, port, servername.to_string()))
    }
}

fn split_host_port(s: &str) -> Result<(&str, u16)> {
    let s = s.trim_end_matches('/');
    if let Some(rest) = s.strip_prefix('[') {
        // IPv6 literal: [::1]:443
        let (host, tail) = rest
            .split_once(']')
            .ok_or_else(|| Error::Config(format!("invalid IPv6 address in `{s}`")))?;
        let port = tail
            .strip_prefix(':')
            .ok_or_else(|| Error::Config(format!("missing port in `{s}`")))?;
        let port: u16 = port
            .parse()
            .map_err(|_| Error::Config(format!("invalid port in `{s}`")))?;
        return Ok((host, port));
    }
    let (host, port) = s
        .rsplit_once(':')
        .ok_or_else(|| Error::Config(format!("missing port in `{s}` (expected host:port)")))?;
    if host.is_empty() {
        return Err(Error::Config(format!("missing host in `{s}`")));
    }
    let port: u16 = port
        .parse()
        .map_err(|_| Error::Config(format!("invalid port in `{s}`")))?;
    if port == 0 {
        return Err(Error::Config(format!("port must not be 0 in `{s}`")));
    }
    Ok((host, port))
}

fn port_of(host_port: &str) -> Result<u16> {
    host_port
        .rsplit_once(':')
        .and_then(|(_, p)| p.parse().ok())
        .ok_or_else(|| Error::Config("invalid host:port".into()))
}

/// Agent TLS trust settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AgentTlsSettings {
    /// Custom root CA bundle (required for self-signed deployments).
    pub ca: Option<PathBuf>,
    /// Optional SPKI SHA-256 pin (base64 or hex).
    pub pin: Option<String>,
    /// TLS SNI/verification host name, defaults to the host in `agent.server`.
    pub servername: Option<String>,
    /// Client certificate for mTLS deployments (when the gateway sets `tls.client_ca`).
    pub client_cert: Option<PathBuf>,
    /// Client private key for mTLS deployments.
    pub client_key: Option<PathBuf>,
    /// Disable certificate verification (debugging only, never in production).
    pub insecure: bool,
}

#[allow(clippy::derivable_impls)]
impl Default for AgentTlsSettings {
    fn default() -> Self {
        AgentTlsSettings {
            ca: None,
            pin: None,
            servername: None,
            client_cert: None,
            client_key: None,
            insecure: false,
        }
    }
}

impl AgentTlsSettings {
    /// Effective SNI.
    pub fn effective_servername(&self, fallback: &str) -> String {
        self.servername
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(fallback)
            .to_string()
    }
}

/// Local model server target.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TargetSettings {
    /// Base URL of the local model server, e.g. `http://127.0.0.1:8000`.
    pub url: String,
    /// Timeout waiting for local response headers.
    #[serde(default = "d_10m", with = "humantime_serde")]
    pub request_timeout: Duration,
    /// Timeout establishing a local connection.
    #[serde(default = "d_5s", with = "humantime_serde")]
    pub connect_timeout: Duration,
    /// Extra CA bundle when the target uses HTTPS with a private CA.
    pub tls_ca: Option<PathBuf>,
    /// Static headers added to every upstream request.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

impl Default for TargetSettings {
    fn default() -> Self {
        TargetSettings {
            url: "http://127.0.0.1:8000".to_string(),
            request_timeout: d_10m(),
            connect_timeout: d_5s(),
            tls_ca: None,
            headers: BTreeMap::new(),
        }
    }
}

/// The complete agent configuration (`agent.toml`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AgentConfig {
    /// Connection + identity.
    pub agent: AgentSettings,
    /// TLS trust.
    pub tls: AgentTlsSettings,
    /// Local model server.
    pub target: TargetSettings,
    /// Logging.
    pub log: LogSettings,
}

#[allow(clippy::derivable_impls)]
impl Default for AgentConfig {
    fn default() -> Self {
        AgentConfig {
            agent: AgentSettings::default(),
            tls: AgentTlsSettings::default(),
            target: TargetSettings::default(),
            log: LogSettings::default(),
        }
    }
}

impl AgentConfig {
    /// Load, apply environment overrides and validate an agent config file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::Config(format!("cannot read {}: {e}", path.display())))?;
        let mut cfg: AgentConfig =
            toml::from_str(&text).map_err(|e| Error::Config(format!("{}: {e}", path.display())))?;
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        cfg.resolve_paths(base);
        cfg.apply_env();
        cfg.validate()?;
        Ok(cfg)
    }

    /// Parse and validate from a TOML string.
    pub fn from_toml_str(text: &str) -> Result<Self> {
        let cfg: AgentConfig = toml::from_str(text)?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Environment overrides for secrets and endpoints:
    /// `DIVERTLINK_SERVER`, `DIVERTLINK_TOKEN`, `DIVERTLINK_ID`.
    pub fn apply_env(&mut self) {
        if let Ok(v) = std::env::var("DIVERTLINK_SERVER") {
            if !v.trim().is_empty() {
                self.agent.server = v;
            }
        }
        if let Ok(v) = std::env::var("DIVERTLINK_TOKEN") {
            if !v.trim().is_empty() {
                self.agent.token = v;
            }
        }
        if let Ok(v) = std::env::var("DIVERTLINK_ID") {
            if !v.trim().is_empty() {
                self.agent.id = v;
            }
        }
    }

    /// Resolve relative certificate paths against `base`.
    pub fn resolve_paths(&mut self, base: &Path) {
        if let Some(p) = &mut self.tls.ca {
            resolve_path(base, p);
        }
        if let Some(p) = &mut self.tls.client_cert {
            resolve_path(base, p);
        }
        if let Some(p) = &mut self.tls.client_key {
            resolve_path(base, p);
        }
        if let Some(p) = &mut self.target.tls_ca {
            resolve_path(base, p);
        }
    }

    /// Validate cross-field constraints.
    pub fn validate(&self) -> Result<()> {
        if self.agent.id.trim().is_empty() {
            return Err(Error::Config("agent.id must not be empty".into()));
        }
        if self.agent.token.trim().is_empty() {
            return Err(Error::Config("agent.token must not be empty".into()));
        }
        self.agent.server_parts()?;
        if self.agent.heartbeat.is_zero() {
            return Err(Error::Config("agent.heartbeat must be > 0".into()));
        }
        if self.agent.heartbeat_timeout <= self.agent.heartbeat {
            return Err(Error::Config(
                "agent.heartbeat_timeout must be greater than agent.heartbeat".into(),
            ));
        }
        if self.agent.max_inflight == 0 {
            return Err(Error::Config("agent.max_inflight must be > 0".into()));
        }
        if self.agent.reconnect_jitter < 0.0 || self.agent.reconnect_jitter > 1.0 {
            return Err(Error::Config(
                "agent.reconnect_jitter must be within 0.0..=1.0".into(),
            ));
        }
        if self.agent.reconnect_max < self.agent.reconnect_min {
            return Err(Error::Config(
                "agent.reconnect_max must be >= agent.reconnect_min".into(),
            ));
        }
        for p in &self.agent.protocols {
            if TunnelProtocol::from_name(p).is_none() {
                return Err(Error::Config(format!(
                    "unknown agent protocol `{p}` (use openai/anthropic/vllm/custom)"
                )));
            }
        }
        let uri: http::Uri = self
            .target
            .url
            .parse()
            .map_err(|e| Error::Config(format!("target.url: {e}")))?;
        if uri.scheme().is_none() || uri.authority().is_none() {
            return Err(Error::Config(
                "target.url must be an absolute URL, e.g. http://127.0.0.1:8000".into(),
            ));
        }
        match uri.scheme_str() {
            Some("http") | Some("https") => {}
            Some(other) => {
                return Err(Error::Config(format!(
                    "target.url scheme `{other}` is not supported (use http/https)"
                )))
            }
            None => return Err(Error::Config("target.url must have a scheme".into())),
        }
        if self.target.request_timeout.is_zero() {
            return Err(Error::Config("target.request_timeout must be > 0".into()));
        }
        if self.target.connect_timeout.is_zero() {
            return Err(Error::Config("target.connect_timeout must be > 0".into()));
        }
        if self.tls.pin.as_deref().is_some_and(|p| p.trim().is_empty()) {
            return Err(Error::Config("tls.pin must not be an empty string".into()));
        }
        match (&self.tls.client_cert, &self.tls.client_key) {
            (Some(_), None) | (None, Some(_)) => {
                return Err(Error::Config(
                    "tls.client_cert and tls.client_key must be set together".into(),
                ))
            }
            _ => {}
        }
        if self.tls.insecure {
            // Allowed, but the caller logs a loud warning at startup.
        }
        Ok(())
    }

    /// A runnable, fully documented example configuration.
    pub fn example() -> &'static str {
        include_str!("../assets/agent.example.toml")
    }

    /// Parsed base URI of the local target.
    pub fn target_uri(&self) -> Result<http::Uri> {
        self.target
            .url
            .parse()
            .map_err(|e| Error::Config(format!("target.url: {e}")))
    }
}

/// Two listeners collide only when they are identical non-ephemeral addresses
/// (`:0` means "let the kernel pick", so equal `:0` entries do not collide).
fn same_listen(a: SocketAddr, b: SocketAddr) -> bool {
    a == b && a.port() != 0
}

fn resolve_path(base: &Path, p: &mut PathBuf) {
    if p.is_relative() && !p.as_os_str().is_empty() {
        *p = base.join(&*p);
    }
}

/// Normalize a mount prefix: leading slash, no trailing slash, rejected traversal.
pub fn normalize_mount(input: &str) -> Result<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(Error::Config("mount must not be empty".into()));
    }
    if !trimmed.starts_with('/') {
        return Err(Error::Config(format!(
            "mount `{trimmed}` must start with '/'"
        )));
    }
    let mut out = trimmed.trim_end_matches('/').to_string();
    if out.is_empty() {
        out.push('/');
    }
    if out
        .split('/')
        .any(|seg| seg == ".." || seg.contains(char::is_whitespace))
    {
        return Err(Error::Config(format!(
            "mount `{trimmed}` contains invalid path segments"
        )));
    }
    Ok(out)
}

/// Normalize a Host header pattern: lowercase, no port.
pub fn normalize_host(input: &str) -> Result<String> {
    let host = input.trim().to_ascii_lowercase();
    if host.is_empty() {
        return Err(Error::Config("host must not be empty".into()));
    }
    if host.contains('/') || host.contains(char::is_whitespace) {
        return Err(Error::Config(format!("invalid host pattern `{input}`")));
    }
    Ok(host)
}

/// Extract the host (no port) from an HTTP Host header value.
pub fn host_without_port(host_header: &str) -> String {
    let h = host_header.trim();
    if let Some(rest) = h.strip_prefix('[') {
        // [::1]:8080
        if let Some((inside, _)) = rest.split_once(']') {
            return inside.to_ascii_lowercase();
        }
    }
    match h.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => host.to_ascii_lowercase(),
        _ => h.to_ascii_lowercase(),
    }
}

/// A non-fatal configuration warning surfaced by `divertlink check` and at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigWarning {
    /// Config field the warning refers to.
    pub field: String,
    /// Human readable explanation and suggested fix.
    pub message: String,
}

macro_rules! lint_push {
    ($warnings:expr, $field:expr, $message:expr $(,)?) => {
        $warnings.push(ConfigWarning {
            field: $field.to_string(),
            message: $message.to_string(),
        })
    };
}

impl std::fmt::Display for ConfigWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}

fn is_loopback_or_private(host: &str) -> bool {
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        Ok(std::net::IpAddr::V6(ip)) => {
            ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local()
        }
        Err(_) => false,
    }
}

fn looks_like_placeholder(secret: &str) -> bool {
    let lower = secret.to_ascii_lowercase();
    lower.contains("replace_me")
        || lower.contains("change_me")
        || lower.contains("changeme")
        || lower == "test"
        || lower == "secret"
        || lower == "password"
        || lower.is_empty()
}

impl ServerConfig {
    /// Non-fatal security/robustness warnings for `divertlink check`.
    pub fn lint(&self) -> Vec<ConfigWarning> {
        let mut warnings = Vec::new();
        if self.api_keys.is_empty() {
            lint_push!(
                warnings,
                "api_keys",
                "no API keys configured: the public ingress is unauthenticated"
            );
        }
        for key in &self.api_keys {
            if key.key.len() < 16 {
                lint_push!(
                    warnings,
                    "api_keys[].key",
                    format!("API key `{}` is shorter than 16 characters", key.name),
                );
            }
            if looks_like_placeholder(&key.key) {
                lint_push!(
                    warnings,
                    "api_keys[].key",
                    format!("API key `{}` looks like a placeholder; rotate it", key.name),
                );
            }
            if key.rate_limit.is_none() && key.max_concurrent == 0 {
                lint_push!(warnings,
                    "api_keys[]",
                    format!(
                        "key `{}` has neither rate_limit nor max_concurrent; a single client can exhaust the tunnel",
                        key.name
                    ),
                );
            }
        }
        for tunnel in &self.tunnels {
            if tunnel.token.len() < 16 || looks_like_placeholder(&tunnel.token) {
                lint_push!(
                    warnings,
                    "tunnels[].token",
                    format!("tunnel `{}` token is weak or a placeholder", tunnel.id),
                );
            }
            if tunnel.settings_no_public_health() {
                lint_push!(
                    warnings,
                    "tunnels[].public_paths",
                    format!(
                        "tunnel `{}` has no public_paths; health checks will need an API key",
                        tunnel.id
                    ),
                );
            }
            if tunnel.upstream_auth_header != UpstreamAuth::Passthrough
                && tunnel.upstream_key().is_none()
            {
                lint_push!(warnings,
                    "tunnels[].upstream_api_key",
                    format!(
                        "tunnel `{}` uses upstream_auth_header = {:?} but upstream_api_key is empty",
                        tunnel.id, tunnel.upstream_auth_header
                    ),
                );
            }
        }
        for route in &self.static_routes {
            if !route.require_auth {
                if let Ok(uri) = route.upstream.parse::<http::Uri>() {
                    let host = uri.host().unwrap_or_default();
                    if !is_loopback_or_private(host) {
                        lint_push!(warnings,
                            "static_routes[].require_auth",
                            format!(
                                "static route `{}` disables gateway auth and forwards to non-private host `{host}`; it becomes an open proxy",
                                route.mount
                            ),
                        );
                    }
                }
            }
        }
        if self.tls.min_version == TlsVersion::V1_2 {
            lint_push!(
                warnings,
                "tls.min_version",
                "TLS 1.2 is allowed; prefer \"1.3\" for agents and modern clients"
            );
        }
        if let Some(admin) = self.server.admin_listen {
            if !admin.ip().is_loopback() {
                lint_push!(warnings,
                    "server.admin_listen",
                    format!("admin endpoints are exposed on {admin}; keep them on loopback or a private network"),
                );
            }
        }
        if self.server.trust_proxy {
            lint_push!(warnings,
                "server.trust_proxy",
                "X-Forwarded-For is trusted: only enable behind a trusted reverse proxy (otherwise allow_ips can be spoofed)",
            );
        }
        if self.limits.max_request_bytes.bytes() > 1024 * 1024 * 1024 {
            lint_push!(
                warnings,
                "limits.max_request_bytes",
                "request body limit above 1 GiB is not recommended"
            );
        }
        match crate::tls::check_cert_expiry(&self.tls.cert) {
            Ok((summary, Some(message))) => lint_push!(
                warnings,
                "tls.cert",
                format!("{message} ({} days left)", summary.days_left)
            ),
            Ok(_) => {}
            Err(err) => lint_push!(
                warnings,
                "tls.cert",
                format!("cannot inspect the certificate: {err}")
            ),
        }
        warnings
    }
}

impl TunnelSettings {
    fn settings_no_public_health(&self) -> bool {
        !self.public_paths.iter().any(|p| p.starts_with("/health"))
    }
}

impl AgentConfig {
    /// Non-fatal security/robustness warnings for `divertlink check`.
    pub fn lint(&self) -> Vec<ConfigWarning> {
        let mut warnings = Vec::new();
        if self.tls.insecure {
            lint_push!(
                warnings,
                "tls.insecure",
                "certificate verification is DISABLED; never use this in production"
            );
        }
        if self.tls.ca.is_none() && !self.tls.insecure {
            lint_push!(warnings,
                "tls.ca",
                "no custom CA configured: the gateway must use a publicly trusted certificate (Let's Encrypt etc.)",
            );
        }
        if self.agent.token.len() < 16 || looks_like_placeholder(&self.agent.token) {
            lint_push!(
                warnings,
                "agent.token",
                "agent token is weak or a placeholder; rotate it"
            );
        }
        if self.agent.heartbeat_mode == HeartbeatMode::None {
            lint_push!(
                warnings,
                "agent.heartbeat_mode",
                "heartbeats are disabled; dead tunnels may take minutes to detect"
            );
        }
        if let Ok(uri) = self.target.url.parse::<http::Uri>() {
            if uri.scheme_str() == Some("http") {
                if let Some(host) = uri.host() {
                    if !is_loopback_or_private(host) {
                        lint_push!(warnings,
                            "target.url",
                            format!("local target `{host}` uses plaintext HTTP over a non-loopback address"),
                        );
                    }
                }
            }
            if uri.scheme_str() == Some("https") && self.target.tls_ca.is_none() {
                lint_push!(
                    warnings,
                    "target.tls_ca",
                    "HTTPS target without a custom CA: the certificate must chain to public roots"
                );
            }
        }
        if self.agent.max_inflight > 1024 {
            lint_push!(
                warnings,
                "agent.max_inflight",
                "very high in-flight limit; each stream consumes memory and a backend slot"
            );
        }
        if let Some(pin) = &self.tls.pin {
            if crate::tls::verify_peer_pin(None, pin).is_err() {
                lint_push!(
                    warnings,
                    "tls.pin",
                    "pin is not a valid 32-byte SHA-256 SPKI digest (base64/hex)"
                );
            }
        }
        warnings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_server_config_is_valid() {
        let mut cfg = ServerConfig::default();
        cfg.tunnels.push(TunnelSettings {
            id: "home".into(),
            token: "token".into(),
            mount: Some("/home".into()),
            hosts: vec![],
            default: false,
            protocols: vec!["openai".into()],
            upstream_api_key: None,
            upstream_auth_header: UpstreamAuth::Passthrough,
            public_paths: vec!["/health".into()],
            max_streams: None,
            request_timeout: None,
            enabled: true,
        });
        cfg.api_keys.push(ApiKeySettings {
            name: "newapi".into(),
            key: "dlk_test".into(),
            allow_tunnels: vec!["home".into()],
            allow_ips: vec!["0.0.0.0/0".into()],
            rate_limit: Some("600/min".into()),
            max_concurrent: 32,
        });
        cfg.validate().unwrap();
        assert_eq!(cfg.limits.max_request_bytes.bytes(), 64 * 1024 * 1024);
        assert_eq!(cfg.limits.max_streams_per_tunnel, 256);
    }

    #[test]
    fn example_server_config_parses_and_validates() {
        let cfg = ServerConfig::from_toml_str(ServerConfig::example())
            .expect("server example must be valid");
        assert!(!cfg.tunnels.is_empty());
        assert!(!cfg.api_keys.is_empty());
    }

    #[test]
    fn example_agent_config_parses_and_validates() {
        let cfg = AgentConfig::from_toml_str(AgentConfig::example())
            .expect("agent example must be valid");
        assert_eq!(cfg.agent.heartbeat, DEFAULT_HEARTBEAT);
        assert_eq!(cfg.agent.heartbeat_timeout, DEFAULT_HEARTBEAT_TIMEOUT);
    }

    #[test]
    fn rejects_duplicate_mounts() {
        let text = r#"
            [[tunnels]]
            id = "a"
            token = "t1"
            mount = "/x"
            [[tunnels]]
            id = "b"
            token = "t2"
            mount = "/x/"
        "#;
        let err = ServerConfig::from_toml_str(text).unwrap_err();
        assert!(err.to_string().contains("duplicate mount"), "{err}");
    }

    #[test]
    fn rejects_duplicate_tunnel_ids() {
        let text = r#"
            [[tunnels]]
            id = "a"
            token = "t1"
            mount = "/a"
            [[tunnels]]
            id = "a"
            token = "t2"
            mount = "/b"
        "#;
        let err = ServerConfig::from_toml_str(text).unwrap_err();
        assert!(err.to_string().contains("duplicate tunnel id"), "{err}");
    }

    #[test]
    fn rejects_unknown_fields() {
        let err = ServerConfig::from_toml_str("[server]\nname = \"x\"\nbogus = 1\n").unwrap_err();
        assert!(err.to_string().to_lowercase().contains("unknown"), "{err}");
    }

    #[test]
    fn rejects_two_default_tunnels() {
        let text = r#"
            [[tunnels]]
            id = "a"
            token = "t1"
            default = true
            [[tunnels]]
            id = "b"
            token = "t2"
            default = true
        "#;
        assert!(ServerConfig::from_toml_str(text).is_err());
    }

    #[test]
    fn listeners_can_be_disabled() {
        let text = r#"
            [server]
            tunnel_listen = "off"
            ingress_listen = "off"
            http_listen = "127.0.0.1:8080"
            admin_listen = "off"
            [tls]
            cert = ""
            key = ""
            [[static_routes]]
            mount = "/newapi"
            upstream = "http://127.0.0.1:3000"
            require_auth = false
        "#;
        let cfg = ServerConfig::from_toml_str(text).unwrap();
        assert!(cfg.server.tunnel_listen.is_none());
        assert!(cfg.server.ingress_listen.is_none());
        assert!(cfg.server.admin_listen.is_none());
        assert_eq!(cfg.server.http_listen.unwrap().port(), 8080);
        // TLS is not needed when no TLS listener is enabled.
        assert!(!cfg.static_routes[0].require_auth);
    }

    #[test]
    fn listeners_accept_false_but_reject_true() {
        let cfg: ServerConfig = toml::from_str("[server]\nhttp_listen = false\n").unwrap();
        assert!(cfg.server.http_listen.is_none());
        assert!(toml::from_str::<ServerConfig>("[server]\nhttp_listen = true\n").is_err());
        assert!(toml::from_str::<ServerConfig>("[server]\nhttp_listen = \"nonsense\"\n").is_err());
    }

    #[test]
    fn static_routes_requiring_auth_need_api_keys() {
        let text = r#"
            [tls]
            cert = ""
            key = ""
            [server]
            tunnel_listen = "off"
            ingress_listen = "off"
            [[static_routes]]
            mount = "/newapi"
            upstream = "http://127.0.0.1:3000"
        "#;
        let err = ServerConfig::from_toml_str(text).unwrap_err();
        assert!(err.to_string().contains("api_keys"), "{err}");
        // With an API key the same config is valid.
        let with_key = format!("{text}\n[[api_keys]]\nname = \"k\"\nkey = \"dlk_test\"\n");
        let cfg = ServerConfig::from_toml_str(&with_key).unwrap();
        assert!(cfg.static_routes[0].require_auth);
    }

    #[test]
    fn lint_flags_insecure_defaults() {
        let text = r#"
            [server]
            tunnel_listen = "off"
            ingress_listen = "off"
            admin_listen = "0.0.0.0:9090"
            [tls]
            cert = ""
            key = ""
            min_version = "1.2"
            [[api_keys]]
            name = "weak"
            key = "short"
            [[tunnels]]
            id = "home"
            token = "weak-token"
            mount = "/home"
        "#;
        let cfg = ServerConfig::from_toml_str(text).unwrap();
        let warnings = cfg.lint();
        let joined = warnings
            .iter()
            .map(|w| w.message.clone())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("TLS 1.2"), "{joined}");
        assert!(joined.contains("shorter than 16"), "{joined}");
        assert!(joined.contains("weak or a placeholder"), "{joined}");
        assert!(joined.contains("admin endpoints are exposed"), "{joined}");
        assert!(joined.contains("neither rate_limit"), "{joined}");
    }

    #[test]
    fn lint_flags_open_static_route_and_private_is_ok() {
        let text = r#"
            [tls]
            cert = ""
            key = ""
            [server]
            tunnel_listen = "off"
            ingress_listen = "off"
            [[api_keys]]
            name = "k"
            key = "dlk_0123456789abcdef0123456789"
            [[static_routes]]
            mount = "/open"
            upstream = "http://203.0.113.5:3000"
            require_auth = false
            [[static_routes]]
            mount = "/local"
            upstream = "http://127.0.0.1:3000"
            require_auth = false
        "#;
        let cfg = ServerConfig::from_toml_str(text).unwrap();
        let warnings = cfg.lint();
        let open: Vec<_> = warnings
            .iter()
            .filter(|w| w.message.contains("open proxy"))
            .collect();
        assert_eq!(
            open.len(),
            1,
            "only the public upstream should warn: {warnings:?}"
        );
        assert!(open[0].message.contains("203.0.113.5"), "{:?}", open[0]);
    }

    #[test]
    fn agent_lint_flags_insecure_and_weak_secrets() {
        let mut cfg = AgentConfig::default();
        cfg.agent.id = "home".into();
        cfg.agent.server = "gw.example.com:443".into();
        cfg.agent.token = "short".into();
        cfg.agent.heartbeat_mode = HeartbeatMode::None;
        cfg.tls.insecure = true;
        cfg.target.url = "http://192.168.1.10:8000".into();
        let warnings = cfg.lint();
        let joined = warnings
            .iter()
            .map(|w| w.message.clone())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("DISABLED"), "{joined}");
        assert!(joined.contains("weak or a placeholder"), "{joined}");
        assert!(joined.contains("heartbeats are disabled"), "{joined}");
    }

    #[test]
    fn agent_server_forms() {
        let mut a = AgentSettings::default();
        for (input, host, port) in [
            ("gw.example.com:443", "gw.example.com", 443),
            ("https://gw.example.com:8443/", "gw.example.com", 8443),
            ("tls://10.0.0.1:9443", "10.0.0.1", 9443),
            ("[::1]:443", "::1", 443),
        ] {
            a.server = input.to_string();
            let (hp, p, sni) = a.server_parts().unwrap();
            assert_eq!(p, port, "{input}");
            assert!(hp.contains(host), "{input}");
            assert!(sni.contains(host) || sni == host, "{input}");
        }
        a.server = "http://gw:80".into();
        assert!(a.server_parts().is_err());
        a.server = "gw".into();
        assert!(a.server_parts().is_err());
    }

    #[test]
    fn agent_validation() {
        let mut cfg = AgentConfig::default();
        cfg.agent.id = "home".into();
        cfg.agent.server = "gw.example.com:443".into();
        cfg.agent.token = "secret".into();
        cfg.validate().unwrap();
        cfg.agent.heartbeat = Duration::from_secs(10);
        cfg.agent.heartbeat_timeout = Duration::from_secs(5);
        assert!(cfg.validate().is_err());
        cfg.agent.heartbeat = d_5s();
        cfg.agent.heartbeat_timeout = d_15s();
        cfg.target.url = "ftp://bad".into();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn mount_normalization() {
        assert_eq!(normalize_mount("/home/").unwrap(), "/home");
        assert_eq!(normalize_mount("/").unwrap(), "/");
        assert!(normalize_mount("home").is_err());
        assert!(normalize_mount("/a/../b").is_err());
        assert!(normalize_mount("/a b").is_err());
    }

    #[test]
    fn host_normalization() {
        assert_eq!(
            normalize_host("Home.Example.COM").unwrap(),
            "home.example.com"
        );
        assert_eq!(
            host_without_port("Home.Example.COM:8443"),
            "home.example.com"
        );
        assert_eq!(host_without_port("home.example.com"), "home.example.com");
        assert_eq!(host_without_port("[::1]:443"), "::1");
        assert!(normalize_host("bad host").is_err());
    }
}
