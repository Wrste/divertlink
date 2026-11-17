//! Lightweight, dependency-free metrics registry with Prometheus text output.
//!
//! We deliberately avoid the `metrics` ecosystem: counters are plain atomics and
//! rendering is a couple of `format!` calls, which keeps the dependency tree
//! small for cross-compiled static binaries.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Instant;

/// Per-tunnel counters.
#[derive(Debug, Default)]
pub struct TunnelStats {
    /// Requests currently in flight.
    pub active: AtomicI64,
    /// Total requests received.
    pub requests: AtomicU64,
    /// Requests that ended with an error status (>=500) or a tunnel failure.
    pub errors: AtomicU64,
    /// Request bytes forwarded towards the model server.
    pub bytes_in: AtomicU64,
    /// Response bytes forwarded towards the client.
    pub bytes_out: AtomicU64,
    /// UNIX seconds of the last successful activity.
    pub last_activity_unix: AtomicU64,
}

impl TunnelStats {
    /// Increment the in-flight gauge (returns a guard? callers pair with `dec_active`).
    pub fn inc_active(&self) {
        self.active.fetch_add(1, Ordering::Relaxed);
    }

    /// Decrement the in-flight gauge.
    pub fn dec_active(&self) {
        self.active.fetch_sub(1, Ordering::Relaxed);
    }

    /// Count one request.
    pub fn inc_request(&self) {
        self.requests.fetch_add(1, Ordering::Relaxed);
    }

    /// Count one failed request.
    pub fn inc_error(&self) {
        self.errors.fetch_add(1, Ordering::Relaxed);
    }

    /// Add request bytes.
    pub fn add_bytes_in(&self, n: u64) {
        self.bytes_in.fetch_add(n, Ordering::Relaxed);
    }

    /// Add response bytes.
    pub fn add_bytes_out(&self, n: u64) {
        self.bytes_out.fetch_add(n, Ordering::Relaxed);
    }

    /// Mark activity now.
    pub fn touch(&self) {
        self.last_activity_unix
            .store(crate::util::unix_now(), Ordering::Relaxed);
    }
}

/// Global metrics registry.
#[derive(Debug)]
pub struct Metrics {
    start: Instant,
    /// Active tunnel connections.
    pub connections_active: AtomicI64,
    /// Total accepted tunnel connections.
    pub connections_total: AtomicU64,
    /// Requests rejected by API key authentication.
    pub auth_failures: AtomicU64,
    /// Requests rejected by rate limiting.
    pub rate_limited: AtomicU64,
    /// Requests rejected because a tunnel had no live session.
    pub tunnel_unavailable: AtomicU64,
    /// Data-stream (HTTP/1.1 over yamux) failures.
    pub stream_errors: AtomicU64,
    /// Heartbeats that timed out and forced a reconnect.
    pub heartbeat_timeouts: AtomicU64,
    /// Agent reconnect attempts (agent side, exported by gateway for ops only if reported).
    pub reconnects: AtomicU64,
    /// Total public requests.
    pub requests_total: AtomicU64,
    /// Total bytes forwarded towards model servers.
    pub bytes_in: AtomicU64,
    /// Total bytes forwarded towards clients.
    pub bytes_out: AtomicU64,
    tunnels: RwLock<HashMap<String, Arc<TunnelStats>>>,
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

impl Metrics {
    /// Create an empty registry.
    pub fn new() -> Self {
        Metrics {
            start: Instant::now(),
            connections_active: AtomicI64::new(0),
            connections_total: AtomicU64::new(0),
            auth_failures: AtomicU64::new(0),
            rate_limited: AtomicU64::new(0),
            tunnel_unavailable: AtomicU64::new(0),
            stream_errors: AtomicU64::new(0),
            heartbeat_timeouts: AtomicU64::new(0),
            reconnects: AtomicU64::new(0),
            requests_total: AtomicU64::new(0),
            bytes_in: AtomicU64::new(0),
            bytes_out: AtomicU64::new(0),
            tunnels: RwLock::new(HashMap::new()),
        }
    }

    /// Uptime.
    pub fn uptime(&self) -> std::time::Duration {
        self.start.elapsed()
    }

    /// Get or create the stats object for a tunnel.
    pub fn tunnel(&self, id: &str) -> Arc<TunnelStats> {
        if let Some(stats) = self
            .tunnels
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
        {
            return stats.clone();
        }
        let mut map = self.tunnels.write().unwrap_or_else(|e| e.into_inner());
        map.entry(id.to_string())
            .or_insert_with(|| Arc::new(TunnelStats::default()))
            .clone()
    }

    /// Remove a tunnel's stats (used when config is reloaded; not in MVP).
    pub fn remove_tunnel(&self, id: &str) {
        self.tunnels
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
    }

    /// Render all metrics in Prometheus text exposition format.
    pub fn render_prometheus(&self) -> String {
        let mut out = String::with_capacity(2048);
        let _ = writeln!(
            out,
            "# HELP divertlink_uptime_seconds Process uptime in seconds"
        );
        let _ = writeln!(out, "# TYPE divertlink_uptime_seconds gauge");
        let _ = writeln!(
            out,
            "divertlink_uptime_seconds {:.3}",
            self.uptime().as_secs_f64()
        );

        let gauges: [(&str, &str, &AtomicI64); 1] = [(
            "divertlink_tunnel_connections_active",
            "Active tunnel connections",
            &self.connections_active,
        )];
        for (name, help, value) in gauges {
            let _ = writeln!(out, "# HELP {name} {help}");
            let _ = writeln!(out, "# TYPE {name} gauge");
            let _ = writeln!(out, "{name} {}", value.load(Ordering::Relaxed));
        }

        let counters: [(&str, &str, &AtomicU64); 9] = [
            (
                "divertlink_tunnel_connections_total",
                "Total tunnel connections",
                &self.connections_total,
            ),
            (
                "divertlink_requests_total",
                "Public API requests",
                &self.requests_total,
            ),
            (
                "divertlink_bytes_in_total",
                "Bytes forwarded to model servers",
                &self.bytes_in,
            ),
            (
                "divertlink_bytes_out_total",
                "Bytes forwarded to clients",
                &self.bytes_out,
            ),
            (
                "divertlink_auth_failures_total",
                "Authentication failures",
                &self.auth_failures,
            ),
            (
                "divertlink_rate_limited_total",
                "Requests rejected by rate limiting",
                &self.rate_limited,
            ),
            (
                "divertlink_tunnel_unavailable_total",
                "Requests with no live tunnel",
                &self.tunnel_unavailable,
            ),
            (
                "divertlink_stream_errors_total",
                "Data stream failures",
                &self.stream_errors,
            ),
            (
                "divertlink_heartbeat_timeouts_total",
                "Heartbeat timeouts",
                &self.heartbeat_timeouts,
            ),
        ];
        for (name, help, value) in counters {
            let _ = writeln!(out, "# HELP {name} {help}");
            let _ = writeln!(out, "# TYPE {name} counter");
            let _ = writeln!(out, "{name} {}", value.load(Ordering::Relaxed));
        }

        // Per-tunnel series.
        let tunnels = {
            let map = self.tunnels.read().unwrap_or_else(|e| e.into_inner());
            map.iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<Vec<_>>()
        };
        let _ = writeln!(
            out,
            "# HELP divertlink_tunnel_requests_total Public requests per tunnel"
        );
        let _ = writeln!(out, "# TYPE divertlink_tunnel_requests_total counter");
        for (id, stats) in &tunnels {
            let _ = writeln!(
                out,
                "divertlink_tunnel_requests_total{{tunnel=\"{}\"}} {}",
                escape_label(id),
                stats.requests.load(Ordering::Relaxed)
            );
        }
        let _ = writeln!(
            out,
            "# HELP divertlink_tunnel_errors_total Failed requests per tunnel"
        );
        let _ = writeln!(out, "# TYPE divertlink_tunnel_errors_total counter");
        for (id, stats) in &tunnels {
            let _ = writeln!(
                out,
                "divertlink_tunnel_errors_total{{tunnel=\"{}\"}} {}",
                escape_label(id),
                stats.errors.load(Ordering::Relaxed)
            );
        }
        let _ = writeln!(
            out,
            "# HELP divertlink_tunnel_active_requests In-flight requests per tunnel"
        );
        let _ = writeln!(out, "# TYPE divertlink_tunnel_active_requests gauge");
        for (id, stats) in &tunnels {
            let _ = writeln!(
                out,
                "divertlink_tunnel_active_requests{{tunnel=\"{}\"}} {}",
                escape_label(id),
                stats.active.load(Ordering::Relaxed)
            );
        }
        let _ = writeln!(
            out,
            "# HELP divertlink_tunnel_bytes_in_total Request bytes per tunnel"
        );
        let _ = writeln!(out, "# TYPE divertlink_tunnel_bytes_in_total counter");
        for (id, stats) in &tunnels {
            let _ = writeln!(
                out,
                "divertlink_tunnel_bytes_in_total{{tunnel=\"{}\"}} {}",
                escape_label(id),
                stats.bytes_in.load(Ordering::Relaxed)
            );
        }
        let _ = writeln!(
            out,
            "# HELP divertlink_tunnel_bytes_out_total Response bytes per tunnel"
        );
        let _ = writeln!(out, "# TYPE divertlink_tunnel_bytes_out_total counter");
        for (id, stats) in &tunnels {
            let _ = writeln!(
                out,
                "divertlink_tunnel_bytes_out_total{{tunnel=\"{}\"}} {}",
                escape_label(id),
                stats.bytes_out.load(Ordering::Relaxed)
            );
        }
        out
    }
}

fn escape_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_expected_series() {
        let m = Metrics::new();
        m.requests_total.fetch_add(3, Ordering::Relaxed);
        let stats = m.tunnel("home");
        stats.inc_request();
        stats.inc_request();
        stats.inc_active();
        let text = m.render_prometheus();
        assert!(text.contains("divertlink_requests_total 3"));
        assert!(text.contains("divertlink_tunnel_requests_total{tunnel=\"home\"} 2"));
        assert!(text.contains("divertlink_tunnel_active_requests{tunnel=\"home\"} 1"));
        assert!(text.contains("divertlink_uptime_seconds"));
    }

    #[test]
    fn label_escaping() {
        let m = Metrics::new();
        let stats = m.tunnel("we\"ird\\name");
        stats.inc_request();
        let text = m.render_prometheus();
        assert!(text.contains("tunnel=\"we\\\"ird\\\\name\""));
    }
}
