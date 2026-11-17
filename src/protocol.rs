//! Control-channel protocol between agent and gateway.
//!
//! The agent opens the first yamux stream as the control stream and speaks
//! newline-delimited JSON over it (one message per line, max
//! [`MAX_CONTROL_LINE`] bytes). Data streams carry plain HTTP/1.1 instead and
//! are fully protocol agnostic.

use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt,
};

use crate::error::{Error, Result};

/// ALPN protocol identifier used by agents on the tunnel connection.
pub const ALPN_TUNNEL: &[u8] = b"divertlink/1";

/// ALPN protocols offered by browser / new-api / SDK clients on the ingress.
pub const ALPN_INGRESS: [&[u8]; 2] = [b"h2", b"http/1.1"];

/// Version string exchanged during the handshake.
pub const PROTOCOL_VERSION: &str = "1";

/// Maximum size of a single control message line.
pub const MAX_CONTROL_LINE: usize = 8192;

/// Maximum accepted `hello.timestamp` drift, in seconds, unless the deployment
/// explicitly disables replay checks (clock skew is common on home equipment).
pub const MAX_HELLO_SKEW_SECS: u64 = 900;

/// Default application heartbeat interval requested by the gateway.
pub const DEFAULT_HEARTBEAT: Duration = Duration::from_secs(5);

/// Default application heartbeat timeout requested by the gateway.
pub const DEFAULT_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(15);

/// Well-known upstream protocol families, used for validation and metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TunnelProtocol {
    /// OpenAI compatible (`/v1/chat/completions`, `/v1/responses`, ...).
    Openai,
    /// Anthropic compatible (`/v1/messages`, `/v1/messages/count_tokens`).
    Anthropic,
    /// vLLM native endpoints (`/health`, `/tokenize`, `/metrics`, ...).
    Vllm,
    /// Any other HTTP API, forwarded transparently.
    Custom,
}

impl TunnelProtocol {
    /// Parse a configuration / hello value.
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "openai" | "openai-compatible" | "oai" => Some(Self::Openai),
            "anthropic" | "claude" => Some(Self::Anthropic),
            "vllm" => Some(Self::Vllm),
            "custom" | "generic" | "http" => Some(Self::Custom),
            _ => None,
        }
    }

    /// Canonical lower-case name.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Openai => "openai",
            Self::Anthropic => "anthropic",
            Self::Vllm => "vllm",
            Self::Custom => "custom",
        }
    }

    /// Guess the protocol family from a request path, for metrics/audit logs.
    pub fn from_path(path: &str) -> Self {
        if path.starts_with("/v1/messages") || path.contains("/messages") {
            Self::Anthropic
        } else if path.starts_with("/health")
            || path.starts_with("/tokenize")
            || path.starts_with("/metrics")
            || path.starts_with("/detokenize")
            || path.starts_with("/pooling")
            || path.starts_with("/classify")
            || path.starts_with("/score")
            || path.starts_with("/rerank")
        {
            Self::Vllm
        } else if path.starts_with("/v1/") {
            Self::Openai
        } else {
            Self::Custom
        }
    }
}

impl fmt::Display for TunnelProtocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Messages exchanged on the control stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlMessage {
    /// Agent -> gateway: register a tunnel.
    Hello {
        /// Configured tunnel id.
        tunnel: String,
        /// Shared secret configured on both ends.
        token: String,
        /// Agent build version, for logs.
        agent_version: String,
        /// Protocol families the agent intends to serve.
        protocols: Vec<String>,
        /// Optional capability flags for forward compatibility.
        capabilities: Vec<String>,
        /// Random nonce, echoed in logs to correlate sessions.
        nonce: String,
        /// UNIX seconds at send time.
        timestamp: u64,
        /// Desired application heartbeat interval (the gateway echoes/adopts it).
        #[serde(default)]
        heartbeat_interval_ms: u64,
        /// Desired application heartbeat timeout.
        #[serde(default)]
        heartbeat_timeout_ms: u64,
    },
    /// Gateway -> agent: handshake accepted.
    HelloAck {
        /// Server-generated session id.
        session: String,
        /// Echo of the tunnel id.
        tunnel: String,
        /// Application heartbeat period requested by the server.
        heartbeat_interval_ms: u64,
        /// Application heartbeat timeout requested by the server.
        heartbeat_timeout_ms: u64,
        /// Maximum number of concurrent data streams.
        max_streams: u32,
    },
    /// Keep-alive probe, either direction.
    Ping { seq: u64 },
    /// Keep-alive answer, either direction.
    Pong { seq: u64 },
    /// Gateway -> agent: graceful or policy driven disconnect.
    Disconnect { reason: String },
    /// Either direction: protocol level error notification.
    Error { code: String, message: String },
}

impl ControlMessage {
    /// Handshake request.
    #[allow(clippy::too_many_arguments)]
    pub fn hello(
        tunnel: impl Into<String>,
        token: impl Into<String>,
        agent_version: impl Into<String>,
        protocols: Vec<String>,
        capabilities: Vec<String>,
    ) -> Self {
        ControlMessage::Hello {
            tunnel: tunnel.into(),
            token: token.into(),
            agent_version: agent_version.into(),
            protocols,
            capabilities,
            nonce: crate::util::hex(&crate::util::random_bytes(8)),
            timestamp: crate::util::unix_now(),
            heartbeat_interval_ms: DEFAULT_HEARTBEAT.as_millis() as u64,
            heartbeat_timeout_ms: DEFAULT_HEARTBEAT_TIMEOUT.as_millis() as u64,
        }
    }

    /// Set the desired heartbeat timings on a hello message.
    pub fn with_heartbeat(mut self, interval: Duration, timeout: Duration) -> Self {
        if let ControlMessage::Hello {
            heartbeat_interval_ms,
            heartbeat_timeout_ms,
            ..
        } = &mut self
        {
            *heartbeat_interval_ms = interval.as_millis().max(1) as u64;
            *heartbeat_timeout_ms = timeout.as_millis().max(2) as u64;
        }
        self
    }

    /// Requested heartbeat interval from a hello message, clamped to sane bounds.
    pub fn requested_heartbeat(&self) -> Option<(Duration, Duration)> {
        match self {
            ControlMessage::Hello {
                heartbeat_interval_ms,
                heartbeat_timeout_ms,
                ..
            } if *heartbeat_interval_ms > 0 && *heartbeat_timeout_ms > 0 => {
                let interval = Duration::from_millis((*heartbeat_interval_ms).clamp(500, 300_000));
                let timeout = Duration::from_millis((*heartbeat_timeout_ms).clamp(1_500, 900_000));
                Some((interval, timeout.max(interval * 2)))
            }
            _ => None,
        }
    }

    /// Handshake response.
    pub fn hello_ack(
        session: impl Into<String>,
        tunnel: impl Into<String>,
        heartbeat: Duration,
        heartbeat_timeout: Duration,
        max_streams: u32,
    ) -> Self {
        ControlMessage::HelloAck {
            session: session.into(),
            tunnel: tunnel.into(),
            heartbeat_interval_ms: heartbeat.as_millis() as u64,
            heartbeat_timeout_ms: heartbeat_timeout.as_millis() as u64,
            max_streams,
        }
    }

    /// Protocol message kind, for logs.
    pub fn kind(&self) -> &'static str {
        match self {
            ControlMessage::Hello { .. } => "hello",
            ControlMessage::HelloAck { .. } => "hello_ack",
            ControlMessage::Ping { .. } => "ping",
            ControlMessage::Pong { .. } => "pong",
            ControlMessage::Disconnect { .. } => "disconnect",
            ControlMessage::Error { .. } => "error",
        }
    }
}

/// Serialize and write one control message followed by `\n`.
pub async fn write_control<W>(writer: &mut W, msg: &ControlMessage) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let mut buf = serde_json::to_vec(msg)?;
    buf.push(b'\n');
    if buf.len() > MAX_CONTROL_LINE {
        return Err(Error::Protocol(format!(
            "control message too large ({} bytes)",
            buf.len()
        )));
    }
    writer.write_all(&buf).await?;
    writer.flush().await?;
    Ok(())
}

/// Read one control message. Returns `Ok(None)` on clean EOF.
pub async fn read_control<R>(reader: &mut R) -> Result<Option<ControlMessage>>
where
    R: AsyncBufRead + AsyncRead + Unpin,
{
    let mut line = String::new();
    let mut limited = (&mut *reader).take((MAX_CONTROL_LINE + 1) as u64);
    let n = limited.read_line(&mut line).await?;
    if n == 0 {
        return Ok(None);
    }
    if line.len() > MAX_CONTROL_LINE {
        return Err(Error::Protocol(format!(
            "control line exceeds {MAX_CONTROL_LINE} bytes"
        )));
    }
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Err(Error::Protocol("empty control line".into()));
    }
    let msg: ControlMessage = serde_json::from_str(trimmed)
        .map_err(|e| Error::Protocol(format!("bad control message: {e}")))?;
    Ok(Some(msg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn control_roundtrip() {
        let hello = ControlMessage::hello("home", "secret", "0.1.0", vec!["openai".into()], vec![]);
        let mut buf: Vec<u8> = Vec::new();
        write_control(&mut buf, &hello).await.unwrap();
        let mut reader = tokio::io::BufReader::new(&buf[..]);
        let got = read_control(&mut reader).await.unwrap().unwrap();
        assert_eq!(got, hello);
        assert!(matches!(got, ControlMessage::Hello { .. }));
        assert_eq!(got.kind(), "hello");
        let rest = read_control(&mut reader).await.unwrap();
        assert!(rest.is_none());
    }

    #[tokio::test]
    async fn rejects_oversized_line() {
        let mut payload = vec![b'x'; MAX_CONTROL_LINE + 10];
        payload.push(b'\n');
        let mut reader = tokio::io::BufReader::new(&payload[..]);
        let err = read_control(&mut reader).await.unwrap_err();
        assert!(matches!(err, Error::Protocol(_)));
    }

    #[tokio::test]
    async fn rejects_garbage() {
        let payload = b"not json\n";
        let mut reader = tokio::io::BufReader::new(&payload[..]);
        let err = read_control(&mut reader).await.unwrap_err();
        assert!(matches!(err, Error::Protocol(_)));
    }

    #[test]
    fn protocol_parsing() {
        assert_eq!(
            TunnelProtocol::from_name("OpenAI"),
            Some(TunnelProtocol::Openai)
        );
        assert_eq!(
            TunnelProtocol::from_name("claude"),
            Some(TunnelProtocol::Anthropic)
        );
        assert_eq!(TunnelProtocol::from_name("nope"), None);
        assert_eq!(
            TunnelProtocol::from_path("/v1/messages"),
            TunnelProtocol::Anthropic
        );
        assert_eq!(
            TunnelProtocol::from_path("/v1/chat/completions"),
            TunnelProtocol::Openai
        );
        assert_eq!(TunnelProtocol::from_path("/tokenize"), TunnelProtocol::Vllm);
        assert_eq!(TunnelProtocol::from_path("/health"), TunnelProtocol::Vllm);
    }

    #[test]
    fn hello_ack_durations() {
        let ack = ControlMessage::hello_ack(
            "sess_x",
            "home",
            Duration::from_secs(5),
            Duration::from_secs(15),
            128,
        );
        match ack {
            ControlMessage::HelloAck {
                heartbeat_interval_ms,
                heartbeat_timeout_ms,
                max_streams,
                ..
            } => {
                assert_eq!(heartbeat_interval_ms, 5000);
                assert_eq!(heartbeat_timeout_ms, 15000);
                assert_eq!(max_streams, 128);
            }
            _ => panic!("wrong variant"),
        }
    }
}
