//! TLS connectivity probe used by the console: connects like an agent would,
//! reports latency, negotiated TLS parameters and certificate details.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustls::pki_types::ServerName;
use serde::{Deserialize, Serialize};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

use crate::error::{Error, Result};
use crate::protocol::ALPN_TUNNEL;
use crate::tls::CertSummary;

/// Options for [`probe_gateway`].
#[derive(Debug, Clone)]
pub struct ProbeOptions {
    /// Host name or IP.
    pub host: String,
    /// TCP port.
    pub port: u16,
    /// Override TLS SNI (defaults to `host`).
    pub servername: Option<String>,
    /// Extra CA bundle (self-signed gateways).
    pub ca: Option<std::path::PathBuf>,
    /// Skip certificate verification (debug only).
    pub insecure: bool,
    /// Optional SPKI pin.
    pub pin: Option<String>,
    /// Overall timeout.
    pub timeout: Duration,
}

/// Probe result, always serializable (failures are reported in `error`).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProbeReport {
    /// Whether the tunnel handshake succeeded.
    pub ok: bool,
    /// Resolved target address.
    pub address: String,
    /// TLS SNI used.
    pub servername: String,
    /// Negotiated TLS version (`TLSv1.3` ...).
    pub tls_version: Option<String>,
    /// Negotiated cipher suite.
    pub cipher: Option<String>,
    /// ALPN selected by the gateway.
    pub alpn: Option<String>,
    /// TCP+TLS handshake latency in milliseconds.
    pub latency_ms: u64,
    /// Leaf certificate summary.
    pub cert: Option<CertSummary>,
    /// Failure description.
    pub error: Option<String>,
}

/// Connect to a gateway exactly like an agent would and report the outcome.
pub async fn probe_gateway(opts: &ProbeOptions) -> ProbeReport {
    let mut report = ProbeReport {
        servername: opts.servername.clone().unwrap_or_else(|| opts.host.clone()),
        ..Default::default()
    };
    let started = Instant::now();
    match probe_inner(opts, &mut report).await {
        Ok(()) => {
            report.ok = true;
            report.latency_ms = started.elapsed().as_millis() as u64;
        }
        Err(err) => {
            report.ok = false;
            report.latency_ms = started.elapsed().as_millis() as u64;
            report.error = Some(err.to_string());
        }
    }
    report
}

async fn probe_inner(opts: &ProbeOptions, report: &mut ProbeReport) -> Result<()> {
    crate::tls::install_default_provider();
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((opts.host.as_str(), opts.port))
        .await
        .map_err(|e| {
            Error::Io(std::io::Error::new(
                e.kind(),
                format!("DNS lookup failed: {e}"),
            ))
        })?
        .collect();
    let addr = addrs
        .first()
        .copied()
        .ok_or_else(|| Error::Unavailable("no address resolved".into()))?;
    report.address = addr.to_string();

    let connect = async {
        let tcp = TcpStream::connect(addr)
            .await
            .map_err(|e| Error::Io(std::io::Error::new(e.kind(), format!("TCP connect: {e}"))))?;
        let _ = tcp.set_nodelay(true);
        let mut settings = crate::config::AgentTlsSettings {
            ca: opts.ca.clone(),
            pin: opts.pin.clone(),
            servername: Some(report.servername.clone()),
            client_cert: None,
            client_key: None,
            insecure: opts.insecure,
        };
        if opts.insecure {
            settings.pin = None;
        }
        let config = crate::tls::build_agent_client_config(&settings)?;
        let connector = TlsConnector::from(config);
        let name = ServerName::try_from(report.servername.clone())
            .map_err(|_| Error::Tls(format!("invalid server name `{}`", report.servername)))?;
        let tls = connector
            .connect(name, tcp)
            .await
            .map_err(|e| Error::Tls(format!("TLS handshake failed: {e}")))?;
        Ok::<_, Error>(tls)
    };

    let tls = tokio::time::timeout(opts.timeout, connect)
        .await
        .map_err(|_| Error::Timeout(format!("probe timed out after {:?}", opts.timeout)))??;

    {
        let (_, connection) = tls.get_ref();
        report.alpn = connection
            .alpn_protocol()
            .map(|p| String::from_utf8_lossy(p).to_string());
        report.tls_version = connection.protocol_version().map(|v| format!("{v:?}"));
        report.cipher = connection
            .negotiated_cipher_suite()
            .map(|c| format!("{:?}", c.suite()));
        if let Some(certs) = connection.peer_certificates() {
            if let Some(leaf) = certs.first() {
                report.cert = crate::tls::cert_summary(leaf.as_ref()).ok();
            }
        }
    }

    // A gateway must negotiate the tunnel ALPN; anything else means the port is
    // serving HTTPS ingress (or not a DivertLink gateway at all).
    match report.alpn.as_deref() {
        Some(value) if value.as_bytes() == ALPN_TUNNEL => Ok(()),
        Some(other) => Err(Error::Protocol(format!(
            "gateway negotiated ALPN `{other}` instead of `divertlink/1`; \
             is this the tunnel port?"
        ))),
        None => Err(Error::Protocol(
            "gateway did not negotiate ALPN (not a DivertLink tunnel port?)".into(),
        )),
    }
}

/// Convenience wrapper used by the UI for `gateway.example.com:443` strings.
pub fn parse_endpoint(input: &str, default_port: u16) -> Result<(String, u16)> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err(Error::Config("endpoint must not be empty".into()));
    }
    let stripped = raw
        .trim_start_matches("tls://")
        .trim_start_matches("https://")
        .trim_end_matches('/');
    if stripped.is_empty() {
        return Err(Error::Config(format!("invalid endpoint `{input}`")));
    }
    // Bracketed IPv6: [::1]:443 or [::1]
    if let Some(rest) = stripped.strip_prefix('[') {
        let (host, tail) = rest
            .split_once(']')
            .ok_or_else(|| Error::Config(format!("invalid IPv6 endpoint `{input}`")))?;
        let port = match tail {
            "" => default_port,
            other => other
                .strip_prefix(':')
                .and_then(|p| p.parse::<u16>().ok())
                .ok_or_else(|| Error::Config(format!("invalid port in `{input}`")))?,
        };
        return Ok((host.to_string(), port));
    }
    // exactly one colon => host:port; zero or many (bare IPv6) => host only.
    match stripped.matches(':').count() {
        0 => Ok((stripped.to_string(), default_port)),
        1 => {
            let (host, port) = stripped.rsplit_once(':').expect("one colon");
            if host.is_empty() {
                return Err(Error::Config(format!("missing host in `{input}`")));
            }
            let port: u16 = port
                .parse()
                .map_err(|_| Error::Config(format!("invalid port in `{input}`")))?;
            if port == 0 {
                return Err(Error::Config(format!("port must not be 0 in `{input}`")));
            }
            Ok((host.to_string(), port))
        }
        _ => Ok((stripped.to_string(), default_port)),
    }
}

/// Probe a config file: reads the agent's gateway or the server's own listeners.
pub async fn probe_config(path: &Path) -> Result<ProbeReport> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| Error::Config(format!("cannot read {}: {e}", path.display())))?;
    if let Ok(agent) = crate::config::AgentConfig::from_toml_str(&text) {
        let (host, port, sni) = agent.agent.server_parts()?;
        let opts = ProbeOptions {
            host,
            port,
            servername: Some(sni),
            ca: agent.tls.ca.clone(),
            insecure: agent.tls.insecure,
            pin: agent.tls.pin.clone(),
            timeout: Duration::from_secs(8),
        };
        return Ok(probe_gateway(&opts).await);
    }
    if let Ok(server) = crate::config::ServerConfig::from_toml_str(&text) {
        // Probe our own listener from loopback, trusting the configured cert.
        let addr = server
            .server
            .tunnel_listen
            .or(server.server.ingress_listen)
            .ok_or_else(|| Error::Config("gateway config has no TLS listener".into()))?;
        let host = if addr.ip().is_unspecified() {
            "127.0.0.1".to_string()
        } else {
            addr.ip().to_string()
        };
        let opts = ProbeOptions {
            host,
            port: addr.port(),
            servername: Some("localhost".into()),
            ca: Some(server.tls.cert.clone()),
            insecure: true,
            pin: None,
            timeout: Duration::from_secs(8),
        };
        return Ok(probe_gateway(&opts).await);
    }
    Err(Error::Config(
        "config is neither agent.toml nor server.toml".into(),
    ))
}

/// A quick TCP-only reachability check (used when TLS material is unavailable).
pub async fn probe_tcp(host: &str, port: u16, timeout: Duration) -> Result<u64> {
    let started = Instant::now();
    let addr = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| Error::Io(std::io::Error::new(e.kind(), e.to_string())))?
        .next()
        .ok_or_else(|| Error::Unavailable("no address resolved".into()))?;
    tokio::time::timeout(timeout, TcpStream::connect(addr))
        .await
        .map_err(|_| Error::Timeout("TCP connect timed out".into()))?
        .map_err(|e| Error::Io(std::io::Error::new(e.kind(), e.to_string())))?;
    Ok(started.elapsed().as_millis() as u64)
}

/// Keep the `Arc` import used when building TLS configuration in future refactors.
#[allow(dead_code)]
fn _arc_marker(_: Arc<()>) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_endpoints() {
        assert_eq!(
            parse_endpoint("gateway.example.com:443", 443).unwrap(),
            ("gateway.example.com".to_string(), 443)
        );
        assert_eq!(
            parse_endpoint("tls://gw.example.com:8443", 443).unwrap(),
            ("gw.example.com".to_string(), 8443)
        );
        assert_eq!(
            parse_endpoint("https://gw.example.com/", 443).unwrap(),
            ("gw.example.com".to_string(), 443)
        );
        assert_eq!(
            parse_endpoint("127.0.0.1", 443).unwrap(),
            ("127.0.0.1".to_string(), 443)
        );
        assert!(parse_endpoint("", 443).is_err());
        assert!(parse_endpoint("host:notaport", 443).is_err());
    }

    #[tokio::test]
    async fn probe_reports_failure_for_closed_port() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let report = probe_gateway(&ProbeOptions {
            host: "127.0.0.1".into(),
            port,
            servername: Some("localhost".into()),
            ca: None,
            insecure: true,
            pin: None,
            timeout: Duration::from_millis(300),
        })
        .await;
        assert!(!report.ok);
        assert!(report.error.is_some());
    }
}
