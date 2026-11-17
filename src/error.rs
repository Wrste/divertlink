//! Error type shared by the library and both ends of the tunnel.

use thiserror::Error;

/// Convenient result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;

/// All errors surfaced by DivertLink.
#[derive(Debug, Error)]
pub enum Error {
    /// Invalid or unusable configuration.
    #[error("configuration error: {0}")]
    Config(String),

    /// Filesystem / socket I/O failure.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// TLS setup or handshake failure.
    #[error("TLS error: {0}")]
    Tls(String),

    /// Wire-protocol violation (bad handshake, oversized control frame, ...).
    #[error("protocol error: {0}")]
    Protocol(String),

    /// The peer closed the connection.
    #[error("connection closed")]
    Closed,

    /// Multiplexer error.
    #[error("multiplexer error: {0}")]
    Yamux(#[from] yamux::ConnectionError),

    /// Hyper transport error.
    #[error("HTTP error: {0}")]
    Http(#[from] hyper::Error),

    /// Malformed HTTP message built by us.
    #[error("HTTP protocol error: {0}")]
    HttpProtocol(#[from] http::Error),

    /// JSON (control channel) error.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// TOML parsing error.
    #[error("TOML error: {0}")]
    Toml(#[from] toml::de::Error),

    /// Authentication or authorization failure.
    #[error("authentication failed: {0}")]
    Auth(String),

    /// A configured limit was hit.
    #[error("limit exceeded: {0}")]
    Limit(String),

    /// An operation ran out of time.
    #[error("timeout: {0}")]
    Timeout(String),

    /// The requested resource/tunnel is not currently available.
    #[error("unavailable: {0}")]
    Unavailable(String),

    /// Mismatch between trusted and presented certificate pin.
    #[error("certificate pin mismatch: {0}")]
    Pin(String),

    /// Internal invariant violation.
    #[error("internal error: {0}")]
    Internal(String),
}

impl Error {
    /// Build an internal error from anything printable.
    pub fn internal(msg: impl Into<String>) -> Self {
        Error::Internal(msg.into())
    }

    /// Build a configuration error from anything printable.
    pub fn config(msg: impl Into<String>) -> Self {
        Error::Config(msg.into())
    }

    /// Build a protocol error from anything printable.
    pub fn protocol(msg: impl Into<String>) -> Self {
        Error::Protocol(msg.into())
    }

    /// Whether this error is fatal to a session / should trigger a reconnect.
    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            Error::Tls(_) | Error::Protocol(_) | Error::Closed | Error::Yamux(_)
        )
    }

    /// Short machine-readable category, for logs and tests.
    pub fn category(&self) -> &'static str {
        match self {
            Error::Config(_) => "config",
            Error::Io(_) => "io",
            Error::Tls(_) => "tls",
            Error::Protocol(_) => "protocol",
            Error::Closed => "closed",
            Error::Yamux(_) => "mux",
            Error::Http(_) => "http",
            Error::HttpProtocol(_) => "http_protocol",
            Error::Json(_) => "json",
            Error::Toml(_) => "toml",
            Error::Auth(_) => "auth",
            Error::Limit(_) => "limit",
            Error::Timeout(_) => "timeout",
            Error::Unavailable(_) => "unavailable",
            Error::Pin(_) => "pin",
            Error::Internal(_) => "internal",
        }
    }
}

impl From<rustls::Error> for Error {
    fn from(e: rustls::Error) -> Self {
        Error::Tls(e.to_string())
    }
}

impl From<tokio::time::error::Elapsed> for Error {
    fn from(e: tokio::time::error::Elapsed) -> Self {
        Error::Timeout(e.to_string())
    }
}

/// Small helper to attach context to `io::Result`.
pub trait IoResultExt<T> {
    /// Add a human readable context message to an I/O result.
    fn context(self, what: impl std::fmt::Display) -> Result<T>;
}

impl<T> IoResultExt<T> for std::io::Result<T> {
    fn context(self, what: impl std::fmt::Display) -> Result<T> {
        self.map_err(|e| Error::Io(std::io::Error::new(e.kind(), format!("{what}: {e}"))))
    }
}
