//! Home-side agent: supervisor loop with exponential backoff and hot reconnect.

pub mod proxy;
pub mod session;

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::config::AgentConfig;
use crate::error::{Error, Result};
use crate::metrics::Metrics;
use crate::upstream::HttpClient;
use crate::util::Backoff;

/// Immutable state shared by every session of one agent process.
#[derive(Debug)]
pub struct AgentState {
    /// Effective configuration.
    pub config: Arc<AgentConfig>,
    /// Parsed local target base URI.
    pub target_uri: axum::http::Uri,
    /// Pooled HTTP client for the local model server.
    pub target_client: HttpClient,
    /// Metrics (connection counters).
    pub metrics: Arc<Metrics>,
    /// Bound on concurrent requests towards the local model server.
    pub inflight: Arc<tokio::sync::Semaphore>,
}

/// A running agent.
pub struct AgentHandle {
    shutdown: CancellationToken,
    task: tokio::task::JoinHandle<()>,
    state: Arc<AgentState>,
}

impl AgentHandle {
    /// Signal the agent to stop (closes the tunnel).
    pub fn cancel(&self) {
        self.shutdown.cancel();
    }

    /// Shared state.
    pub fn state(&self) -> Arc<AgentState> {
        self.state.clone()
    }

    /// Cancel and wait for the supervisor to finish.
    pub async fn shutdown(self) {
        self.shutdown.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(5), self.task).await;
    }
}

/// Build the shared agent state from configuration.
pub fn build_state(config: AgentConfig) -> Result<Arc<AgentState>> {
    config.validate()?;
    if config.tls.insecure {
        warn!("tls.insecure is enabled: the gateway certificate will NOT be verified");
    }
    if config.tls.client_cert.is_some() {
        info!("agent will present a client certificate for mutual TLS");
    }
    for warning in config.lint() {
        warn!("config warning: {warning}");
    }
    if config.agent.heartbeat_mode == crate::config::HeartbeatMode::None {
        warn!("heartbeat_mode = none: dead tunnels may take minutes to be detected");
    }
    crate::tls::install_default_provider();
    let (_, _, _) = config.agent.server_parts()?;
    let target_uri = config.target_uri()?;
    let target_client = crate::upstream::build_http_client(
        config.target.tls_ca.as_deref(),
        config.target.connect_timeout,
    )?;
    let metrics = Arc::new(Metrics::new());
    let config = Arc::new(config);
    Ok(Arc::new(AgentState {
        inflight: Arc::new(tokio::sync::Semaphore::new(
            config.agent.max_inflight.max(1),
        )),
        config: config.clone(),
        target_uri,
        target_client,
        metrics,
    }))
}

/// Connect once and serve until the session ends (used by `--once` and tests).
pub async fn run_once(config: AgentConfig) -> Result<()> {
    let state = build_state(config)?;
    session::run_session(state, CancellationToken::new()).await
}

/// Start the agent supervisor. Returns once the supervisor task is scheduled, so
/// tests can observe immediate reconnect behavior.
pub async fn run(config: AgentConfig, shutdown: Option<CancellationToken>) -> Result<AgentHandle> {
    let shutdown = shutdown.unwrap_or_default();
    let state = build_state(config)?;
    let supervisor_state = state.clone();
    let supervisor_shutdown = shutdown.clone();
    let task = tokio::spawn(async move {
        supervisor(supervisor_state, supervisor_shutdown).await;
    });

    Ok(AgentHandle {
        shutdown,
        task,
        state,
    })
}

async fn supervisor(state: Arc<AgentState>, shutdown: CancellationToken) {
    let config = state.config.clone();
    let mut backoff = Backoff::new(
        config.agent.reconnect_min,
        config.agent.reconnect_max,
        config.agent.reconnect_jitter,
    );
    let mut attempts: u64 = 0;
    loop {
        if shutdown.is_cancelled() {
            return;
        }
        attempts += 1;
        let started = Instant::now();
        let result = session::run_session(state.clone(), shutdown.clone()).await;
        let session_duration = started.elapsed();
        state
            .metrics
            .connections_active
            .store(0, std::sync::atomic::Ordering::Relaxed);
        match result {
            Ok(()) => {
                if shutdown.is_cancelled() {
                    info!("agent stopped");
                    return;
                }
                info!("tunnel session closed by gateway");
            }
            Err(err) => {
                warn!(
                    attempt = attempts,
                    error = %err,
                    category = err.category(),
                    "tunnel session failed"
                );
                if matches!(err, Error::Auth(_) | Error::Pin(_)) {
                    warn!("authentication/pinning failure: not retrying with the same credentials in a tight loop; backing off");
                }
            }
        }
        if session_duration > Duration::from_secs(30) {
            // The connection was healthy for a while: reset the backoff and count.
            backoff.reset();
            attempts = 0;
        }
        if shutdown.is_cancelled() {
            return;
        }
        let delay = backoff.next_delay();
        info!(
            delay_ms = delay.as_millis() as u64,
            "reconnecting after backoff"
        );
        state
            .metrics
            .reconnects
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = tokio::time::sleep(delay) => {}
        }
    }
}

/// Convenience for embedding: run the agent until Ctrl-C.
pub async fn run_until_signal(config: AgentConfig) -> Result<()> {
    let handle = run(config, None).await?;
    info!("divertlink agent running; press Ctrl-C to stop");
    crate::signal::shutdown_signal().await;
    info!("shutdown signal received");
    handle.shutdown().await;
    Ok(())
}

/// Log an unrecoverable agent error.
pub fn log_fatal(err: &Error, context: &str) {
    error!(error = %err, %context, "agent stopped");
}
