//! Gateway TLS session with a single agent: yamux driver, handshake, control loop.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::future::poll_fn;
use tokio::io::{AsyncBufRead, AsyncWrite, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use tokio_rustls::server::TlsStream;
use tokio_util::compat::{FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::config::ServerConfig;
use crate::error::{Error, Result};
use crate::metrics::Metrics;
use crate::protocol::{
    read_control, write_control, ControlMessage, MAX_HELLO_SKEW_SECS, PROTOCOL_VERSION,
};
use crate::registry::{ActiveSession, OpenRequest, Registry};
use crate::util::{session_id, unix_now};

/// How long the agent has to present a valid control stream + hello.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum number of messages buffered from the control stream reader.
const CONTROL_QUEUE: usize = 64;

/// Handle one accepted TLS connection that negotiated `divertlink/1`.
pub async fn handle_connection(
    tls: TlsStream<TcpStream>,
    peer: SocketAddr,
    registry: Arc<Registry>,
    metrics: Arc<Metrics>,
    config: Arc<ServerConfig>,
    shutdown: CancellationToken,
) {
    match run_session(
        tls,
        peer,
        registry.clone(),
        metrics.clone(),
        config,
        shutdown,
    )
    .await
    {
        Ok(()) => debug!(%peer, "tunnel session ended"),
        Err(err) => {
            warn!(%peer, error = %err, category = err.category(), "tunnel session failed");
            if matches!(err, Error::Auth(_)) {
                metrics
                    .auth_failures
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }
}

async fn run_session(
    tls: TlsStream<TcpStream>,
    peer: SocketAddr,
    registry: Arc<Registry>,
    metrics: Arc<Metrics>,
    config: Arc<ServerConfig>,
    shutdown: CancellationToken,
) -> Result<()> {
    let mux_io = tls.compat();
    let mut mux_config = yamux::Config::default();
    let max_streams = (config.limits.max_streams_per_tunnel * 2).clamp(512, 4096);
    mux_config.set_max_num_streams(max_streams);
    mux_config.set_split_send_size(64 * 1024);
    let conn = yamux::Connection::new(mux_io, mux_config, yamux::Mode::Server);

    let cancel = CancellationToken::new();
    let (open_tx, open_rx) = ActiveSession::channels();
    let last_seen = Arc::new(std::sync::atomic::AtomicU64::new(unix_now()));
    let (control_tx, control_rx) = oneshot::channel();

    // The driver must poll the multiplexer during the handshake, otherwise the
    // SYN/hello frames queued by both ends are never flushed.
    let driver = tokio::spawn(drive_connection(
        conn,
        open_rx,
        control_tx,
        cancel.clone(),
        shutdown.clone(),
    ));

    let control = timeout(HANDSHAKE_TIMEOUT, control_rx)
        .await
        .map_err(|_| Error::Timeout("agent did not open a control stream".into()))?
        .map_err(|_| Error::Closed)?;

    let tokio_control = control.compat();
    let (read_half, write_half) = tokio::io::split(tokio_control);
    let mut reader = BufReader::new(read_half);
    let mut writer = write_half;

    let hello = timeout(HANDSHAKE_TIMEOUT, read_control(&mut reader))
        .await
        .map_err(|_| Error::Timeout("agent hello timed out".into()))??
        .ok_or_else(|| Error::Protocol("control stream closed before hello".into()))?;

    let (tunnel_id, agent_version, protocols, capabilities, nonce, requested) = match hello {
        ControlMessage::Hello {
            tunnel,
            token,
            agent_version,
            protocols,
            capabilities,
            nonce,
            timestamp,
            ..
        } => {
            let entry = match registry.tunnel(&tunnel) {
                Some(entry) => entry,
                None => {
                    let _ = write_control(
                        &mut writer,
                        &ControlMessage::Error {
                            code: "unknown_tunnel".into(),
                            message: format!("unknown or disabled tunnel `{tunnel}`"),
                        },
                    )
                    .await;
                    return Err(Error::Auth(format!(
                        "unknown tunnel `{tunnel}` from {peer}"
                    )));
                }
            };
            if !crate::auth::verify_token(&entry.settings.token, &token) {
                let _ = write_control(
                    &mut writer,
                    &ControlMessage::Error {
                        code: "invalid_token".into(),
                        message: "agent token rejected".into(),
                    },
                )
                .await;
                return Err(Error::Auth(format!(
                    "invalid token for tunnel `{tunnel}` from {peer}"
                )));
            }
            let now = unix_now();
            let skew = now.abs_diff(timestamp);
            if skew > MAX_HELLO_SKEW_SECS {
                warn!(%peer, tunnel = %tunnel, skew_secs = skew, "agent clock skew is large");
            }
            let requested = ControlMessage::Hello {
                tunnel: tunnel.clone(),
                token: String::new(),
                agent_version: agent_version.clone(),
                protocols: protocols.clone(),
                capabilities: capabilities.clone(),
                nonce: nonce.clone(),
                timestamp,
                heartbeat_interval_ms: 0,
                heartbeat_timeout_ms: 0,
            }
            .requested_heartbeat();
            (
                tunnel,
                agent_version,
                protocols,
                capabilities,
                nonce,
                requested,
            )
        }
        other => {
            let _ = write_control(
                &mut writer,
                &ControlMessage::Error {
                    code: "expected_hello".into(),
                    message: format!("expected hello, got {}", other.kind()),
                },
            )
            .await;
            return Err(Error::Protocol(format!(
                "expected hello, got {}",
                other.kind()
            )));
        }
    };

    let (heartbeat_interval, heartbeat_timeout) = requested.unwrap_or((
        crate::protocol::DEFAULT_HEARTBEAT,
        crate::protocol::DEFAULT_HEARTBEAT_TIMEOUT,
    ));
    let entry = registry
        .tunnel(&tunnel_id)
        .ok_or_else(|| Error::Auth("tunnel disappeared".into()))?;
    let session_id = session_id();

    let ack = ControlMessage::hello_ack(
        session_id.clone(),
        tunnel_id.clone(),
        heartbeat_interval,
        heartbeat_timeout,
        entry.max_streams as u32,
    );
    write_control(&mut writer, &ack).await?;

    let session = ActiveSession::new(
        session_id.clone(),
        tunnel_id.clone(),
        agent_version.clone(),
        protocols.clone(),
        capabilities,
        peer,
        entry.max_streams as u32,
        cancel,
        open_tx,
        last_seen,
    );
    let replaced = registry.install_session(&tunnel_id, session.clone())?;
    metrics
        .connections_total
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // Gauge guard: decremented once this session (and its spawned tasks) unwind.
    let _active = ActiveConnGuard::new(metrics.clone());
    info!(
        %peer,
        tunnel = %tunnel_id,
        session = %session_id,
        agent_version = %agent_version,
        nonce = %nonce,
        protocols = ?protocols,
        replaced_previous = replaced.is_some(),
        "agent connected"
    );

    // Reader task: parse control messages into a bounded queue so cancellation of
    // the select loop below can never lose a partially read line.
    let (msg_tx, mut msg_rx) = mpsc::channel(CONTROL_QUEUE);
    let reader_handle = tokio::spawn(async move {
        loop {
            match read_control(&mut reader).await {
                Ok(Some(msg)) => {
                    if msg_tx.send(msg).await.is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(err) => {
                    debug!(error = %err, "control stream read ended");
                    break;
                }
            }
        }
    });

    let outcome = control_loop(
        &mut writer,
        &mut msg_rx,
        &session,
        heartbeat_interval,
        heartbeat_timeout,
        &metrics,
        &shutdown,
    )
    .await;

    session.cancel.cancel();
    driver.abort();
    reader_handle.abort();
    registry.remove_session(&tunnel_id, &session_id);
    info!(%peer, tunnel = %tunnel_id, session = %session_id, reason = ?outcome, "agent disconnected");
    Ok(())
}

/// Decrements the active-connection gauge on drop.
struct ActiveConnGuard(Arc<Metrics>);

impl ActiveConnGuard {
    fn new(metrics: Arc<Metrics>) -> Self {
        metrics
            .connections_active
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        ActiveConnGuard(metrics)
    }
}

impl Drop for ActiveConnGuard {
    fn drop(&mut self) {
        self.0
            .connections_active
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ControlEnd {
    Shutdown,
    PeerClosed,
    HeartbeatTimeout,
    ProtocolError,
    WriteFailed,
}

async fn control_loop<W>(
    writer: &mut W,
    msg_rx: &mut mpsc::Receiver<ControlMessage>,
    session: &Arc<ActiveSession>,
    heartbeat_interval: Duration,
    heartbeat_timeout: Duration,
    metrics: &Metrics,
    shutdown: &CancellationToken,
) -> ControlEnd
where
    W: AsyncWrite + Unpin,
{
    let mut last_seen = Instant::now();
    let mut ticker = tokio::time::interval(heartbeat_interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await; // consume immediate first tick
    let mut seq: u64 = 0;
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                let _ = write_control(writer, &ControlMessage::Disconnect {
                    reason: "gateway shutting down".into(),
                }).await;
                return ControlEnd::Shutdown;
            }
            _ = session.cancel.cancelled() => {
                return ControlEnd::PeerClosed;
            }
            _ = ticker.tick() => {
                if last_seen.elapsed() > heartbeat_timeout {
                    metrics.heartbeat_timeouts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    warn!(session = %session.id, "heartbeat timeout, dropping session");
                    return ControlEnd::HeartbeatTimeout;
                }
                seq += 1;
                if write_control(writer, &ControlMessage::Ping { seq }).await.is_err() {
                    return ControlEnd::WriteFailed;
                }
            }
            msg = msg_rx.recv() => {
                let Some(msg) = msg else { return ControlEnd::PeerClosed };
                last_seen = Instant::now();
                session.touch();
                match msg {
                    ControlMessage::Ping { seq } => {
                        if write_control(writer, &ControlMessage::Pong { seq }).await.is_err() {
                            return ControlEnd::WriteFailed;
                        }
                    }
                    ControlMessage::Pong { .. } => {}
                    ControlMessage::Disconnect { reason } => {
                        info!(session = %session.id, %reason, "agent requested disconnect");
                        return ControlEnd::PeerClosed;
                    }
                    ControlMessage::Error { code, message } => {
                        warn!(session = %session.id, %code, %message, "agent reported error");
                    }
                    other => {
                        warn!(session = %session.id, kind = other.kind(), "unexpected control message");
                        return ControlEnd::ProtocolError;
                    }
                }
            }
        }
    }
}

/// Drive the multiplexer: forward the agent's first inbound stream (the control
/// stream) to the handshake task, open outbound data streams on request, and
/// reject any further inbound streams (agents never open data streams).
async fn drive_connection(
    mut conn: yamux::Connection<
        impl futures_util::AsyncRead + futures_util::AsyncWrite + Unpin + Send,
    >,
    mut open_rx: mpsc::Receiver<OpenRequest>,
    control_tx: oneshot::Sender<yamux::Stream>,
    cancel: CancellationToken,
    shutdown: CancellationToken,
) {
    let mut control_tx = Some(control_tx);
    let mut opened_streams = 0u64;
    loop {
        tokio::select! {
            biased;
            request = open_rx.recv() => {
                let Some(request) = request else { break };
                match poll_fn(|cx| conn.poll_new_outbound(cx)).await {
                    Ok(stream) => {
                        opened_streams = opened_streams.saturating_add(1);
                        let _ = request.respond.send(Ok(stream));
                    }
                    Err(err) => {
                        let _ = request.respond.send(Err(format!("cannot open stream: {err}")));
                        break;
                    }
                }
            }
            inbound = poll_fn(|cx| conn.poll_next_inbound(cx)) => {
                match inbound {
                    Some(Ok(stream)) => {
                        if let Some(tx) = control_tx.take() {
                            if tx.send(stream).is_err() {
                                break;
                            }
                        } else {
                            warn!(
                                opened_streams,
                                "agent opened an unexpected inbound stream; closing it"
                            );
                            drop(stream);
                        }
                    }
                    Some(Err(err)) => {
                        debug!(error = %err, "yamux connection error");
                        break;
                    }
                    None => break,
                }
            }
            _ = shutdown.cancelled() => break,
            _ = cancel.cancelled() => break,
        }
    }
    cancel.cancel();
}

/// Helper retained for tests: read the first control message of a fresh stream.
pub async fn read_hello<R: AsyncBufRead + Unpin>(reader: &mut R) -> Result<ControlMessage> {
    timeout(HANDSHAKE_TIMEOUT, read_control(reader))
        .await
        .map_err(|_| Error::Timeout("hello timeout".into()))??
        .ok_or_else(|| Error::Protocol("control closed before hello".into()))
}

/// Silence unused import warnings for `oneshot` (used via `OpenRequest`).
#[allow(dead_code)]
fn _assert_oneshot(_: oneshot::Sender<()>) {}

/// Protocol version advertised by this gateway.
pub fn protocol_version() -> &'static str {
    PROTOCOL_VERSION
}
