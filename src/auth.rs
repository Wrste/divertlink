//! API key authentication, token verification and per-key concurrency limits.

use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::http::{header, HeaderMap, StatusCode};
use ipnet::IpNet;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::config::ApiKeySettings;
use crate::error::{Error, Result};
use crate::ratelimit::{RateLimiter, RateSpec};

/// Header used by Anthropic clients.
pub const X_API_KEY: &str = "x-api-key";

/// Marker extension carrying the direct peer address of an HTTP request.
#[derive(Debug, Clone, Copy)]
pub struct PeerAddr(pub IpAddr);

/// One configured API key with its parsed policy.
#[derive(Debug)]
pub struct KeyEntry {
    /// Key name for audit logs.
    pub name: String,
    hash: [u8; 32],
    allowed_tunnels: Vec<String>,
    allowed_ips: Vec<IpNet>,
    rate: Option<RateLimiter>,
    max_concurrent: usize,
    inflight: AtomicUsize,
}

impl KeyEntry {
    /// Anonymous key used when ingress authentication is disabled.
    pub fn anonymous() -> Self {
        KeyEntry {
            name: "anonymous".to_string(),
            hash: [0; 32],
            allowed_tunnels: Vec::new(),
            allowed_ips: Vec::new(),
            rate: None,
            max_concurrent: 0,
            inflight: AtomicUsize::new(0),
        }
    }

    fn from_config(cfg: &ApiKeySettings) -> Result<Self> {
        let mut allowed_ips = Vec::new();
        for cidr in &cfg.allow_ips {
            allowed_ips.push(
                cidr.parse::<IpNet>()
                    .map_err(|e| Error::Config(format!("invalid CIDR `{cidr}`: {e}")))?,
            );
        }
        let rate = match &cfg.rate_limit {
            Some(spec) if !spec.trim().is_empty() => {
                let spec: RateSpec = spec.parse()?;
                Some(RateLimiter::new(spec))
            }
            _ => None,
        };
        Ok(KeyEntry {
            name: cfg.name.clone(),
            hash: hash_secret(&cfg.key),
            allowed_tunnels: cfg.allow_tunnels.clone(),
            allowed_ips,
            rate,
            max_concurrent: cfg.max_concurrent,
            inflight: AtomicUsize::new(0),
        })
    }

    /// Rate limit specification for status output.
    pub fn rate_spec(&self) -> Option<RateSpec> {
        self.rate.as_ref().map(|r| r.spec())
    }

    /// Check the per-key rate limit.
    pub fn check_rate(&self) -> std::result::Result<(), AuthError> {
        match &self.rate {
            Some(limiter) => limiter.check().map_err(AuthError::RateLimited),
            None => Ok(()),
        }
    }

    /// Check that the key may access a tunnel.
    pub fn authorize_tunnel(&self, tunnel_id: &str) -> std::result::Result<(), AuthError> {
        if self.allowed_tunnels.is_empty() || self.allowed_tunnels.iter().any(|t| t == tunnel_id) {
            Ok(())
        } else {
            Err(AuthError::TunnelDenied)
        }
    }

    /// Acquire a concurrency slot for this key (returned guard releases it on drop).
    pub fn acquire(self: &Arc<Self>) -> std::result::Result<KeyGuard, AuthError> {
        if self.max_concurrent == 0 {
            return Ok(KeyGuard {
                entry: None,
                released: false,
            });
        }
        let mut current = self.inflight.load(Ordering::Relaxed);
        loop {
            if current >= self.max_concurrent {
                return Err(AuthError::Busy);
            }
            match self.inflight.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    return Ok(KeyGuard {
                        entry: Some(self.clone()),
                        released: false,
                    })
                }
                Err(actual) => current = actual,
            }
        }
    }
}

/// RAII guard for an in-flight slot.
#[derive(Debug)]
pub struct KeyGuard {
    entry: Option<Arc<KeyEntry>>,
    released: bool,
}

impl KeyGuard {
    /// Release explicitly (also happens on drop).
    pub fn release(mut self) {
        self.release_inner();
        self.released = true;
    }

    fn release_inner(&mut self) {
        if let Some(entry) = self.entry.take() {
            entry.inflight.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

impl Drop for KeyGuard {
    fn drop(&mut self) {
        if !self.released {
            self.release_inner();
        }
    }
}

/// A store of configured API keys.
#[derive(Debug)]
pub struct KeyStore {
    keys: Vec<Arc<KeyEntry>>,
}

impl KeyStore {
    /// Build from configuration. Empty stores mean "authentication disabled".
    pub fn from_config(keys: &[ApiKeySettings]) -> Result<Self> {
        let keys = keys
            .iter()
            .map(KeyEntry::from_config)
            .collect::<Result<Vec<_>>>()?;
        Ok(KeyStore {
            keys: keys.into_iter().map(Arc::new).collect(),
        })
    }

    /// Whether authentication is enabled.
    pub fn is_enabled(&self) -> bool {
        !self.keys.is_empty()
    }

    /// Number of configured keys.
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Authenticate a request by headers + source IP.
    ///
    /// Every key digest is compared in constant time regardless of the presented
    /// credential, so the response time does not reveal which key matched or
    /// whether a prefix was correct.
    pub fn authenticate(
        &self,
        headers: &HeaderMap,
        client_ip: IpAddr,
    ) -> std::result::Result<Arc<KeyEntry>, AuthError> {
        if self.keys.is_empty() {
            // No keys configured: authentication is disabled (single-user setup).
            // The gateway logs a warning at startup in this case.
            return Ok(Arc::new(KeyEntry::anonymous()));
        }
        let presented = extract_api_key(headers).ok_or(AuthError::Missing)?;
        let digest = hash_secret(presented);
        let mut matched: Option<&Arc<KeyEntry>> = None;
        for entry in &self.keys {
            let eq: bool = entry.hash.ct_eq(&digest).into();
            if eq {
                matched = Some(entry);
            }
        }
        let entry = matched.ok_or(AuthError::Invalid)?;
        if !entry.allowed_ips.is_empty()
            && !entry.allowed_ips.iter().any(|net| net.contains(&client_ip))
        {
            return Err(AuthError::IpDenied);
        }
        Ok(entry.clone())
    }
}

/// Result of failed authentication/authorization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    /// No credential was presented.
    Missing,
    /// The credential is not valid.
    Invalid,
    /// The source IP is not allowed for this key.
    IpDenied,
    /// The key is not allowed to access the requested tunnel.
    TunnelDenied,
    /// Rate limited, with a suggested retry delay.
    RateLimited(Duration),
    /// Too many concurrent requests for this key.
    Busy,
}

impl AuthError {
    /// HTTP status code for ingress responses.
    pub fn status(&self) -> StatusCode {
        match self {
            AuthError::Missing | AuthError::Invalid => StatusCode::UNAUTHORIZED,
            AuthError::IpDenied | AuthError::TunnelDenied => StatusCode::FORBIDDEN,
            AuthError::RateLimited(_) | AuthError::Busy => StatusCode::TOO_MANY_REQUESTS,
        }
    }

    /// Stable machine-readable code for JSON error bodies and audit logs.
    pub fn code(&self) -> &'static str {
        match self {
            AuthError::Missing => "missing_api_key",
            AuthError::Invalid => "invalid_api_key",
            AuthError::IpDenied => "ip_not_allowed",
            AuthError::TunnelDenied => "tunnel_not_allowed",
            AuthError::RateLimited(_) => "rate_limited",
            AuthError::Busy => "too_many_concurrent_requests",
        }
    }

    /// Suggested `Retry-After` value.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            AuthError::RateLimited(d) => Some(*d),
            _ => None,
        }
    }
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::Missing => f.write_str("missing API key"),
            AuthError::Invalid => f.write_str("invalid API key"),
            AuthError::IpDenied => f.write_str("client IP not allowed for this key"),
            AuthError::TunnelDenied => f.write_str("API key is not allowed to access this tunnel"),
            AuthError::RateLimited(d) => write!(f, "rate limited (retry in {d:?})"),
            AuthError::Busy => f.write_str("too many concurrent requests"),
        }
    }
}

impl std::error::Error for AuthError {}

/// Extract an API key from `Authorization: Bearer <key>` or `x-api-key: <key>`.
pub fn extract_api_key(headers: &HeaderMap) -> Option<&str> {
    if let Some(value) = headers.get(header::AUTHORIZATION) {
        if let Ok(value) = value.to_str() {
            if let Some(rest) = value
                .strip_prefix("Bearer ")
                .or_else(|| value.strip_prefix("bearer "))
            {
                let rest = rest.trim();
                if !rest.is_empty() {
                    return Some(rest);
                }
            }
        }
    }
    if let Some(value) = headers.get(X_API_KEY) {
        if let Ok(value) = value.to_str() {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    None
}

/// SHA-256 digest of a secret.
pub fn hash_secret(secret: &str) -> [u8; 32] {
    Sha256::digest(secret.as_bytes()).into()
}

/// Constant-time comparison of two secrets of arbitrary length.
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    let a = hash_secret(a);
    let b = hash_secret(b);
    a.ct_eq(&b).into()
}

/// Verify an agent token in constant time.
pub fn verify_token(expected: &str, provided: &str) -> bool {
    constant_time_eq(expected, provided)
}

/// Resolve the effective client IP, optionally trusting proxy headers.
pub fn client_ip(peer: IpAddr, headers: &HeaderMap, trust_proxy: bool) -> IpAddr {
    if !trust_proxy {
        return peer;
    }
    if let Some(value) = headers.get("x-forwarded-for") {
        if let Ok(value) = value.to_str() {
            if let Some(first) = value.split(',').next() {
                if let Ok(ip) = first.trim().parse::<IpAddr>() {
                    return ip;
                }
            }
        }
    }
    if let Some(value) = headers.get("x-real-ip") {
        if let Ok(value) = value.to_str() {
            if let Ok(ip) = value.trim().parse::<IpAddr>() {
                return ip;
            }
        }
    }
    peer
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn settings(name: &str, key: &str) -> ApiKeySettings {
        ApiKeySettings {
            name: name.to_string(),
            key: key.to_string(),
            allow_tunnels: Vec::new(),
            allow_ips: Vec::new(),
            rate_limit: None,
            max_concurrent: 0,
        }
    }

    fn store() -> KeyStore {
        KeyStore::from_config(&[settings("one", "dlk_one"), settings("two", "dlk_two")]).unwrap()
    }

    fn bearer(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        headers
    }

    #[test]
    fn extracts_bearer_and_x_api_key() {
        let headers = bearer("abc");
        assert_eq!(extract_api_key(&headers), Some("abc"));
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", HeaderValue::from_static("xyz"));
        assert_eq!(extract_api_key(&headers), Some("xyz"));
        assert_eq!(extract_api_key(&HeaderMap::new()), None);
    }

    #[test]
    fn authenticates_known_keys() {
        let store = store();
        let entry = store
            .authenticate(&bearer("dlk_two"), "127.0.0.1".parse().unwrap())
            .unwrap();
        assert_eq!(entry.name, "two");
        assert_eq!(
            store
                .authenticate(&bearer("dlk_bad"), "127.0.0.1".parse().unwrap())
                .unwrap_err(),
            AuthError::Invalid
        );
        assert_eq!(
            store
                .authenticate(&HeaderMap::new(), "127.0.0.1".parse().unwrap())
                .unwrap_err(),
            AuthError::Missing
        );
    }

    #[test]
    fn ip_allowlist() {
        let mut cfg = settings("net", "dlk_net");
        cfg.allow_ips = vec!["10.0.0.0/8".into()];
        let store = KeyStore::from_config(&[cfg]).unwrap();
        assert!(store
            .authenticate(&bearer("dlk_net"), "10.1.2.3".parse().unwrap())
            .is_ok());
        assert_eq!(
            store
                .authenticate(&bearer("dlk_net"), "192.168.1.1".parse().unwrap())
                .unwrap_err(),
            AuthError::IpDenied
        );
    }

    #[test]
    fn tunnel_scope() {
        let mut cfg = settings("scoped", "dlk_scoped");
        cfg.allow_tunnels = vec!["home".into()];
        let entry = KeyEntry::from_config(&cfg).unwrap();
        assert!(entry.authorize_tunnel("home").is_ok());
        assert_eq!(
            entry.authorize_tunnel("office").unwrap_err(),
            AuthError::TunnelDenied
        );
        let unscoped = KeyEntry::from_config(&settings("all", "dlk_all")).unwrap();
        assert!(unscoped.authorize_tunnel("anything").is_ok());
    }

    #[test]
    fn rate_limit_is_enforced() {
        let mut cfg = settings("limited", "dlk_limited");
        cfg.rate_limit = Some("2/min".into());
        let store = KeyStore::from_config(&[cfg]).unwrap();
        let entry = store
            .authenticate(&bearer("dlk_limited"), "127.0.0.1".parse().unwrap())
            .unwrap();
        assert!(entry.check_rate().is_ok());
        assert!(entry.check_rate().is_ok());
        match entry.check_rate() {
            Err(AuthError::RateLimited(_)) => {}
            other => panic!("expected rate limit, got {other:?}"),
        }
    }

    #[test]
    fn concurrency_guard_releases() {
        let mut cfg = settings("c", "dlk_c");
        cfg.max_concurrent = 1;
        let entry = Arc::new(KeyEntry::from_config(&cfg).unwrap());
        let guard = entry.acquire().unwrap();
        assert_eq!(entry.acquire().unwrap_err(), AuthError::Busy);
        drop(guard);
        assert!(entry.acquire().is_ok());
    }

    #[test]
    fn constant_time_token_checks() {
        assert!(verify_token("secret-token", "secret-token"));
        assert!(!verify_token("secret-token", "secret-toke"));
        assert!(!verify_token("", "anything"));
    }

    #[test]
    fn proxy_ip_resolution() {
        let peer: IpAddr = "127.0.0.1".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("203.0.113.7, 10.0.0.1"),
        );
        assert_eq!(client_ip(peer, &headers, false), peer);
        assert_eq!(
            client_ip(peer, &headers, true),
            "203.0.113.7".parse::<IpAddr>().unwrap()
        );
    }
}
