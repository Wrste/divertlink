//! One agent session: TLS connect, yamux client, handshake, control loop.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::future::poll_fn;
use tokio::io::{AsyncBufRead, AsyncWrite, BufReader};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_rustls::TlsConnector;
use tokio_util::compat::{FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::proxy;
use super::AgentState;
use crate::error::{Error, Result};
use crate::protocol::{read_control, write_control, ControlMessage, ALPN_TUNNEL, PROTOCOL_VERSION};
use crate::util::{random_token, unix_now};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const CONTROL_QUEUE: usize = 64;

/// Establish a tunnel session and serve it until it closes or `shutdown` fires.
pub async fn run_session(state: Arc<AgentState>, shutdown: CancellationToken) -> Result<()> {
    let config = state.config.clone();
    let (host_port, _port, default_sni) = config.agent.server_parts()?;
    let sni = config.tls.effective_servername(&default_sni);

    // DNS resolution (may return several addresses; try them in order).
    let addrs = timeout(
        config.agent.connect_timeout,
        tokio::net::lookup_host(host_port.as_str()),
    )
    .await
    .map_err(|_| Error::Timeout(format!("DNS resolution for {host_port} timed out")))?
    .map_err(|e| {
        Error::Io(std::io::Error::new(
            e.kind(),
            format!("DNS lookup {host_port}: {e}"),
        ))
    })?
    .collect::<Vec<_>>();
    if addrs.is_empty() {
        return Err(Error::Unavailable(format!(
            "no addresses resolved for {host_port}"
        )));
    }

    let mut last_err = None;
    let mut tcp = None;
    for addr in addrs {
        debug!(%addr, "connecting to gateway");
        let keepalive = if config.agent.heartbeat_mode.transport_enabled() {
            Some(config.agent.heartbeat.max(Duration::from_secs(10)))
        } else {
            None
        };
        match crate::net::tcp_connect(addr, config.agent.connect_timeout, keepalive).await {
            Ok(stream) => {
                tcp = Some(stream);
                break;
            }
            Err(err) => {
                debug!(%addr, error = %err, "connect failed");
                last_err = Some(err);
            }
        }
    }
    let tcp = tcp.ok_or_else(|| {
        last_err.unwrap_or_else(|| Error::Unavailable(format!("cannot connect to {host_port}")))
    })?;

    // TLS handshake with ALPN divertlink/1.
    let client_config = crate::tls::build_agent_client_config(&config.tls)?;
    let connector = TlsConnector::from(client_config);
    let server_name = crate::tls::server_name(&sni)?;
    let tls = timeout(
        config.agent.connect_timeout,
        connector.connect(server_name, tcp),
    )
    .await
    .map_err(|_| Error::Timeout("TLS handshake timed out".into()))?
    .map_err(|e| Error::Tls(format!("TLS handshake failed: {e}")))?;

    let negotiated = tls
        .get_ref()
        .1
        .alpn_protocol()
        .map(|p| p.to_vec())
        .unwrap_or_default();
    if negotiated.as_slice() != ALPN_TUNNEL {
        return Err(Error::Protocol(format!(
            "gateway did not select the divertlink tunnel protocol (ALPN {:?}); \
             is the server port correct?",
            String::from_utf8_lossy(&negotiated)
        )));
    }
    info!(gateway = %host_port, sni = %sni, "TLS connection established");

    let mux_io = tls.compat();
    let mut mux_config = yamux::Config::default();
    mux_config.set_max_num_streams((config.agent.max_inflight * 2).clamp(512, 4096));
    mux_config.set_split_send_size(64 * 1024);
    let mut conn = yamux::Connection::new(mux_io, mux_config, yamux::Mode::Client);

    // The agent always opens the control stream first.
    let control = timeout(HANDSHAKE_TIMEOUT, poll_fn(|cx| conn.poll_new_outbound(cx)))
        .await
        .map_err(|_| Error::Timeout("timed out opening control stream".into()))?
        .map_err(Error::Yamux)?;

    // Start pumping the multiplexer before the handshake: yamux only flushes
    // queued frames (such as hello) and reads frames while it is being polled.
    let driver_shutdown = shutdown.clone();
    let driver_state = state.clone();
    let driver = tokio::spawn(async move {
        drive_connection(conn, driver_state, driver_shutdown).await;
    });

    let tokio_control = control.compat();
    let (read_half, write_half) = tokio::io::split(tokio_control);
    let mut reader = BufReader::new(read_half);
    let mut writer = write_half;

    let hello = ControlMessage::hello(
        config.agent.id.clone(),
        config.agent.token.clone(),
        format!("divertlink-agent/{}", env!("CARGO_PKG_VERSION")),
        config.agent.protocols.clone(),
        vec![
            format!("protocol/{PROTOCOL_VERSION}"),
            "stream/http1.1".to_string(),
        ],
    )
    .with_heartbeat(config.agent.heartbeat, config.agent.heartbeat_timeout);
    write_control(&mut writer, &hello).await?;

    let ack = timeout(HANDSHAKE_TIMEOUT, read_control(&mut reader))
        .await
        .map_err(|_| Error::Timeout("gateway did not answer the hello in time".into()))??
        .ok_or(Error::Closed)?;
    let (session_id, ack_interval, ack_timeout, max_streams) = match ack {
        ControlMessage::HelloAck {
            session,
            heartbeat_interval_ms,
            heartbeat_timeout_ms,
            max_streams,
            ..
        } => (
            session,
            Duration::from_millis(heartbeat_interval_ms.max(1)),
            Duration::from_millis(heartbeat_timeout_ms.max(2)),
            max_streams,
        ),
        ControlMessage::Error { code, message } => {
            return Err(Error::Auth(format!(
                "gateway rejected the agent: {code}: {message}"
            )))
        }
        other => {
            return Err(Error::Protocol(format!(
                "expected hello_ack, got {}",
                other.kind()
            )))
        }
    };

    // Effective heartbeat: send often enough for both sides, fail fast on silence.
    let heartbeat_interval = config
        .agent
        .heartbeat
        .min(ack_interval.max(Duration::from_millis(500)));
    let heartbeat_timeout = config.agent.heartbeat_timeout.min(ack_timeout);
    if heartbeat_timeout <= heartbeat_interval {
        warn!(
            ?heartbeat_interval,
            ?heartbeat_timeout,
            "heartbeat timeout is not larger than the interval; using interval * 3"
        );
    }
    let heartbeat_timeout = heartbeat_timeout.max(heartbeat_interval * 3);

    if config.agent.max_inflight as u32 > max_streams {
        warn!(
            max_inflight = config.agent.max_inflight,
            gateway_max_streams = max_streams,
            "agent.max_inflight exceeds the gateway stream limit; the gateway limit wins"
        );
    }
    info!(
        session = %session_id,
        tunnel = %config.agent.id,
        max_streams,
        heartbeat_ms = heartbeat_interval.as_millis() as u64,
        timeout_ms = heartbeat_timeout.as_millis() as u64,
        "tunnel session established"
    );
    state
        .metrics
        .connections_active
        .fetch_add(1, Ordering::Relaxed);
    state
        .metrics
        .connections_total
        .fetch_add(1, Ordering::Relaxed);

    // Control reader task: parse into a queue so cancellation never loses data.
    let (msg_tx, mut msg_rx) = mpsc::channel(CONTROL_QUEUE);
    let reader_task = tokio::spawn(async move {
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

    let end = control_loop(
        &mut writer,
        &mut msg_rx,
        heartbeat_interval,
        heartbeat_timeout,
        &shutdown,
    )
    .await;

    driver.abort();
    reader_task.abort();
    state.metrics.connections_active.store(0, Ordering::Relaxed);
    info!(session = %session_id, reason = ?end, "tunnel session ending");
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum End {
    Shutdown,
    PeerClosed,
    HeartbeatTimeout,
    WriteFailed,
    ProtocolError,
}

async fn control_loop<W>(
    writer: &mut W,
    msg_rx: &mut mpsc::Receiver<ControlMessage>,
    heartbeat_interval: Duration,
    heartbeat_timeout: Duration,
    shutdown: &CancellationToken,
) -> End
where
    W: AsyncWrite + Unpin,
{
    let mut last_seen = Instant::now();
    let mut ticker = tokio::time::interval(heartbeat_interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await;
    let mut seq: u64 = 0;
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                let _ = write_control(writer, &ControlMessage::Disconnect {
                    reason: "agent shutting down".into(),
                }).await;
                return End::Shutdown;
            }
            _ = ticker.tick() => {
                if last_seen.elapsed() > heartbeat_timeout {
                    warn!("heartbeat timeout: no control message from the gateway");
                    return End::HeartbeatTimeout;
                }
                seq += 1;
                if write_control(writer, &ControlMessage::Ping { seq }).await.is_err() {
                    return End::WriteFailed;
                }
            }
            msg = msg_rx.recv() => {
                let Some(msg) = msg else { return End::PeerClosed };
                last_seen = Instant::now();
                match msg {
                    ControlMessage::Ping { seq } => {
                        if write_control(writer, &ControlMessage::Pong { seq }).await.is_err() {
                            return End::WriteFailed;
                        }
                    }
                    ControlMessage::Pong { .. } => {}
                    ControlMessage::Disconnect { reason } => {
                        info!(%reason, "gateway requested disconnect");
                        return End::PeerClosed;
                    }
                    ControlMessage::Error { code, message } => {
                        warn!(%code, %message, "gateway reported an error");
                    }
                    other => {
                        warn!(kind = other.kind(), "unexpected control message from gateway");
                        return End::ProtocolError;
                    }
                }
            }
        }
    }
}

async fn drive_connection(
    mut conn: yamux::Connection<
        impl futures_util::AsyncRead + futures_util::AsyncWrite + Unpin + Send,
    >,
    state: Arc<AgentState>,
    shutdown: CancellationToken,
) {
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            inbound = poll_fn(|cx| conn.poll_next_inbound(cx)) => {
                match inbound {
                    Some(Ok(stream)) => {
                        let state = state.clone();
                        let Ok(permit) = state.inflight.clone().try_acquire_owned() else {
                            warn!("agent inflight limit reached; refusing a data stream");
                            continue;
                        };
                        tokio::spawn(async move {
                            let _permit = permit;
                            proxy::serve_stream(stream, state).await;
                        });
                    }
                    Some(Err(err)) => {
                        debug!(error = %err, "yamux error");
                        break;
                    }
                    None => break,
                }
            }
        }
    }
}

/// Send a disconnect notice on the control stream (used on graceful shutdown).
pub async fn graceful_disconnect<W: AsyncWrite + Unpin>(writer: &mut W, reason: &str) {
    let _ = write_control(
        writer,
        &ControlMessage::Disconnect {
            reason: reason.to_string(),
        },
    )
    .await;
}

/// Generate a nonce for diagnostics.
pub fn connection_nonce() -> String {
    random_token("", 6)
}

/// Current timestamp helper used by tests.
pub fn now_secs() -> u64 {
    unix_now()
}

/// Read the next control message with a timeout (test helper).
pub async fn next_control<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    wait: Duration,
) -> Result<Option<ControlMessage>> {
    timeout(wait, read_control(reader))
        .await
        .map_err(|_| Error::Timeout("control read timeout".into()))?
}
