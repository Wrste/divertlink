//! Gateway: TLS accept, ALPN demultiplexing, ingress/admin listeners.

pub mod admin;
pub mod ingress;
pub mod session;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::Request;
use axum::Router;
use hyper_util::rt::TokioExecutor;
use hyper_util::rt::TokioIo;
use hyper_util::server::conn::auto::Builder as AutoBuilder;
use hyper_util::service::TowerToHyperService;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::auth::{KeyStore, PeerAddr};
use crate::config::ServerConfig;
use crate::error::{Error, Result};
use crate::metrics::Metrics;
use crate::protocol::ALPN_TUNNEL;
use crate::registry::Registry;

/// Shared state for the ingress and admin routers.
pub struct ServerState {
    /// Effective configuration.
    pub config: Arc<ServerConfig>,
    /// Tunnel/route registry.
    pub registry: Arc<Registry>,
    /// Public API keys.
    pub keys: Arc<KeyStore>,
    /// Process metrics.
    pub metrics: Arc<Metrics>,
    /// Prebuilt ingress router (auth + routing + forwarding).
    pub ingress_router: Router,
}

impl std::fmt::Debug for ServerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerState")
            .field("config", &self.config.server.name)
            .field("keys", &self.keys.len())
            .finish()
    }
}

/// Bound listener addresses, useful for tests and startup logs.
#[derive(Debug, Clone, Copy, Default)]
pub struct ServerAddrs {
    /// Agent-facing TLS listener.
    pub tunnel: Option<SocketAddr>,
    /// Public HTTPS listener.
    pub ingress: Option<SocketAddr>,
    /// Plaintext HTTP listener.
    pub http: Option<SocketAddr>,
    /// Admin listener.
    pub admin: Option<SocketAddr>,
}

/// A running gateway.
pub struct ServerHandle {
    addrs: ServerAddrs,
    state: Arc<ServerState>,
    shutdown: CancellationToken,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl ServerHandle {
    /// Bound listener addresses.
    pub fn addrs(&self) -> ServerAddrs {
        self.addrs
    }

    /// Shared state (registry, metrics).
    pub fn state(&self) -> Arc<ServerState> {
        self.state.clone()
    }

    /// Signal all listeners to stop without waiting.
    pub fn cancel(&self) {
        self.shutdown.cancel();
    }

    /// Cancel and wait for listener tasks to finish.
    pub async fn shutdown(mut self) {
        self.shutdown.cancel();
        for task in self.tasks.drain(..) {
            let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
        }
    }
}

/// Which protocols a TLS listener accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TlsRole {
    /// Accept agents and public clients on the same port (ALPN demultiplexing).
    Combined,
    /// Accept agents only (mTLS capable, dedicated port).
    TunnelOnly,
    /// Accept public HTTPS clients only.
    IngressOnly,
}

/// Start the gateway. Returns once all listeners are bound.
pub async fn run(
    config: ServerConfig,
    shutdown: Option<CancellationToken>,
) -> Result<ServerHandle> {
    config.validate()?;
    if config.api_keys.is_empty() {
        warn!(
            "ingress authentication is disabled: no [[api_keys]] configured \
             (validation only allows this without tunnels and with static routes set to require_auth = false)"
        );
    }
    if config.tls.client_ca.is_some() {
        info!("mutual TLS enabled for agent connections");
    }
    crate::tls::install_default_provider();
    let shutdown = shutdown.unwrap_or_default();
    // Agents may be required to present a client certificate (mTLS), but public
    // clients must not be. Build a separate ingress TLS config with client auth
    // disabled and HTTP-only ALPN. TLS material is only required when a TLS
    // listener is enabled, so pure plaintext deployments work without certs.
    let tunnel_tls = if config.server.tunnel_listen.is_some() {
        Some(Arc::new(TlsAcceptor::from(
            crate::tls::build_server_config(&config.tls)?,
        )))
    } else {
        None
    };
    let ingress_tls = if config.server.ingress_listen.is_some() {
        let mut ingress_tls_settings = config.tls.clone();
        ingress_tls_settings.client_ca = None;
        Some(Arc::new(TlsAcceptor::from(
            crate::tls::build_server_config_with_alpn(
                &ingress_tls_settings,
                crate::protocol::ALPN_INGRESS
                    .iter()
                    .map(|p| p.to_vec())
                    .collect(),
            )?,
        )))
    } else {
        None
    };
    let metrics = Arc::new(Metrics::new());
    let registry = Registry::new(&config, metrics.clone())?;
    let keys = Arc::new(KeyStore::from_config(&config.api_keys)?);
    let config = Arc::new(config);

    let ingress_router = ingress::router(Arc::new(ServerState {
        config: config.clone(),
        registry: registry.clone(),
        keys: keys.clone(),
        metrics: metrics.clone(),
        ingress_router: Router::new(),
    }));
    // Rebuild with the real router: ServerState must contain the router used by
    // both the TLS and plaintext listeners.
    let state = Arc::new(ServerState {
        config: config.clone(),
        registry,
        keys,
        metrics: metrics.clone(),
        ingress_router,
    });

    let tunnel_acceptor = tunnel_tls.clone();
    let ingress_acceptor = ingress_tls.clone();
    let mut tasks = Vec::new();
    let mut addrs = ServerAddrs::default();

    let tunnel_addr = config.server.tunnel_listen;
    let ingress_addr = config.server.ingress_listen;
    let shared = match (tunnel_addr, ingress_addr) {
        (Some(t), Some(i)) => t == i,
        _ => false,
    };

    if shared {
        let addr = tunnel_addr.expect("shared listener address");
        let listener = bind(addr, "tunnel/ingress").await?;
        let bound = listener.local_addr()?;
        addrs.tunnel = Some(bound);
        addrs.ingress = Some(bound);
        info!(
            addr = %bound,
            "listening for agents (divertlink/1) and HTTPS clients (h2/http1.1) on the same port"
        );
        tasks.push(spawn_tls_accept(
            listener,
            tunnel_acceptor
                .clone()
                .ok_or_else(|| Error::Config("shared listener requires TLS material".into()))?,
            TlsRole::Combined,
            state.clone(),
            shutdown.clone(),
            config.limits.max_connections,
        ));
    } else {
        if let Some(addr) = tunnel_addr {
            let listener = bind(addr, "tunnel").await?;
            addrs.tunnel = Some(listener.local_addr()?);
            info!(addr = %addrs.tunnel.unwrap(), "listening for agent tunnel connections");
            tasks.push(spawn_tls_accept(
                listener,
                tunnel_acceptor
                    .clone()
                    .ok_or_else(|| Error::Config("tunnel listener requires TLS material".into()))?,
                TlsRole::TunnelOnly,
                state.clone(),
                shutdown.clone(),
                config.limits.max_connections,
            ));
        }
        if let Some(addr) = ingress_addr {
            let listener = bind(addr, "ingress").await?;
            addrs.ingress = Some(listener.local_addr()?);
            info!(addr = %addrs.ingress.unwrap(), "listening for public HTTPS requests");
            tasks.push(spawn_tls_accept(
                listener,
                ingress_acceptor.clone().ok_or_else(|| {
                    Error::Config("ingress listener requires TLS material".into())
                })?,
                TlsRole::IngressOnly,
                state.clone(),
                shutdown.clone(),
                config.limits.max_connections,
            ));
        }
    }

    if let Some(addr) = config.server.http_listen {
        let listener = bind(addr, "http").await?;
        addrs.http = Some(listener.local_addr()?);
        info!(addr = %addrs.http.unwrap(), "listening for plaintext HTTP requests");
        tasks.push(spawn_http_accept(
            listener,
            state.clone(),
            shutdown.clone(),
            config.limits.max_connections,
        ));
    }

    if let Some(addr) = config.server.admin_listen {
        let listener = bind(addr, "admin").await?;
        addrs.admin = Some(listener.local_addr()?);
        info!(addr = %addrs.admin.unwrap(), "listening for admin endpoints (/healthz /readyz /metrics /status)");
        let router = admin::router(state.clone());
        tasks.push(spawn_http_router(
            listener,
            router,
            shutdown.clone(),
            config.limits.max_connections,
        ));
    }

    if addrs.tunnel.is_none() && addrs.ingress.is_none() && addrs.http.is_none() {
        return Err(Error::Config(
            "no ingress listener configured: set server.tunnel_listen, ingress_listen or http_listen"
                .into(),
        ));
    }

    Ok(ServerHandle {
        addrs,
        state,
        shutdown,
        tasks,
    })
}

async fn bind(addr: SocketAddr, what: &str) -> Result<TcpListener> {
    TcpListener::bind(addr).await.map_err(|e| {
        Error::Io(std::io::Error::new(
            e.kind(),
            format!("cannot bind {what} {addr}: {e}"),
        ))
    })
}

fn spawn_tls_accept(
    listener: TcpListener,
    acceptor: Arc<TlsAcceptor>,
    role: TlsRole,
    state: Arc<ServerState>,
    shutdown: CancellationToken,
    max_connections: usize,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let limit = Arc::new(tokio::sync::Semaphore::new(max_connections.max(1)));
        loop {
            let accepted = tokio::select! {
                _ = shutdown.cancelled() => break,
                res = listener.accept() => res,
            };
            let (tcp, peer) = match accepted {
                Ok(pair) => pair,
                Err(err) => {
                    warn!(error = %err, "accept failed");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            };
            let Ok(permit) = limit.clone().try_acquire_owned() else {
                debug!(%peer, "connection limit reached, rejecting");
                drop(tcp);
                continue;
            };
            let acceptor = acceptor.clone();
            let state = state.clone();
            let shutdown = shutdown.clone();
            tokio::spawn(async move {
                let _permit = permit;
                let tls = match tokio::time::timeout(Duration::from_secs(10), acceptor.accept(tcp))
                    .await
                {
                    Ok(Ok(tls)) => tls,
                    Ok(Err(err)) => {
                        debug!(%peer, error = %err, "TLS handshake failed");
                        return;
                    }
                    Err(_) => {
                        debug!(%peer, "TLS handshake timed out");
                        return;
                    }
                };
                let alpn = tls.get_ref().1.alpn_protocol().map(|p| p.to_vec());
                match alpn.as_deref() {
                    Some(ALPN_TUNNEL) => {
                        if role == TlsRole::IngressOnly {
                            warn!(%peer, "agent tried to tunnel on the public ingress port; rejecting");
                            return;
                        }
                        session::handle_connection(
                            tls,
                            peer,
                            state.registry.clone(),
                            state.metrics.clone(),
                            state.config.clone(),
                            shutdown,
                        )
                        .await;
                    }
                    _ => {
                        if role == TlsRole::TunnelOnly {
                            warn!(%peer, alpn = ?alpn, "non-tunnel client on tunnel port; rejecting");
                            return;
                        }
                        serve_ingress(tls, peer, state, shutdown).await;
                    }
                }
            });
        }
        debug!("TLS listener stopped");
    })
}

fn spawn_http_accept(
    listener: TcpListener,
    state: Arc<ServerState>,
    shutdown: CancellationToken,
    max_connections: usize,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let limit = Arc::new(tokio::sync::Semaphore::new(max_connections.max(1)));
        loop {
            let accepted = tokio::select! {
                _ = shutdown.cancelled() => break,
                res = listener.accept() => res,
            };
            let (tcp, peer) = match accepted {
                Ok(pair) => pair,
                Err(err) => {
                    warn!(error = %err, "accept failed");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            };
            let Ok(permit) = limit.clone().try_acquire_owned() else {
                drop(tcp);
                continue;
            };
            let state = state.clone();
            let shutdown = shutdown.clone();
            tokio::spawn(async move {
                let _permit = permit;
                serve_ingress(tcp, peer, state, shutdown).await;
            });
        }
        debug!("plaintext listener stopped");
    })
}

fn spawn_http_router(
    listener: TcpListener,
    router: Router,
    shutdown: CancellationToken,
    max_connections: usize,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let limit = Arc::new(tokio::sync::Semaphore::new(max_connections.max(1)));
        loop {
            let accepted = tokio::select! {
                _ = shutdown.cancelled() => break,
                res = listener.accept() => res,
            };
            let (tcp, peer) = match accepted {
                Ok(pair) => pair,
                Err(err) => {
                    warn!(error = %err, "accept failed");
                    continue;
                }
            };
            let Ok(permit) = limit.clone().try_acquire_owned() else {
                drop(tcp);
                continue;
            };
            let router = router.clone();
            let shutdown = shutdown.clone();
            tokio::spawn(async move {
                let _permit = permit;
                serve_router(tcp, peer, router, shutdown).await;
            });
        }
        debug!("admin listener stopped");
    })
}

/// Serve the ingress router over a plain tokio stream (TLS or TCP).
async fn serve_ingress<I>(
    io: I,
    peer: SocketAddr,
    state: Arc<ServerState>,
    shutdown: CancellationToken,
) where
    I: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let router = state.ingress_router.clone();
    let service = PeerInjector::new(router, peer);
    let hyper_service = TowerToHyperService::new(service);
    let mut builder = AutoBuilder::new(TokioExecutor::new());
    builder
        .http1()
        .timer(hyper_util::rt::TokioTimer::new())
        .header_read_timeout(Some(state.config.limits.header_read_timeout));
    let conn = builder.serve_connection(TokioIo::new(io), hyper_service);
    tokio::pin!(conn);
    tokio::select! {
        result = conn.as_mut() => {
            if let Err(err) = result {
                debug!(%peer, error = %err, "ingress connection closed with error");
            }
        }
        _ = shutdown.cancelled() => {
            conn.as_mut().graceful_shutdown();
            let _ = tokio::time::timeout(Duration::from_secs(2), conn).await;
        }
    }
}

async fn serve_router<I>(io: I, peer: SocketAddr, router: Router, shutdown: CancellationToken)
where
    I: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let service = PeerInjector::new(router, peer);
    let hyper_service = TowerToHyperService::new(service);
    let mut builder = AutoBuilder::new(TokioExecutor::new());
    builder
        .http1()
        .timer(hyper_util::rt::TokioTimer::new())
        .header_read_timeout(Some(Duration::from_secs(10)));
    let conn = builder.serve_connection(TokioIo::new(io), hyper_service);
    tokio::pin!(conn);
    tokio::select! {
        result = conn.as_mut() => {
            if let Err(err) = result {
                debug!(%peer, error = %err, "admin connection closed with error");
            }
        }
        _ = shutdown.cancelled() => {
            conn.as_mut().graceful_shutdown();
            let _ = tokio::time::timeout(Duration::from_secs(2), conn).await;
        }
    }
}

/// Tower service that injects the peer address into request extensions.
#[derive(Clone)]
struct PeerInjector<S> {
    inner: S,
    peer: SocketAddr,
}

impl<S> PeerInjector<S> {
    fn new(inner: S, peer: SocketAddr) -> Self {
        PeerInjector { inner, peer }
    }
}

impl<S, B> tower::Service<Request<B>> for PeerInjector<S>
where
    S: tower::Service<Request<B>, Response = axum::response::Response> + Clone + Send + 'static,
    S::Future: Send + 'static,
    B: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::result::Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut request: Request<B>) -> Self::Future {
        request.extensions_mut().insert(PeerAddr(self.peer.ip()));
        self.inner.call(request)
    }
}

/// Convenience for embedding the gateway: start with Ctrl-C handling.
pub async fn run_until_signal(config: ServerConfig) -> Result<()> {
    let handle = run(config, None).await?;
    let addrs = handle.addrs();
    info!(?addrs, "divertlink gateway started");
    tokio::signal::ctrl_c()
        .await
        .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))?;
    info!("shutdown signal received");
    handle.shutdown().await;
    Ok(())
}

/// Bind a test listener on an ephemeral port (helper for tests).
pub async fn ephemeral_addr() -> Result<SocketAddr> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    drop(listener);
    Ok(addr)
}

/// Log a fatal listener error.
pub fn log_fatal(err: &Error, context: &str) {
    error!(error = %err, %context, "gateway stopped");
}

/// Silence unused import when `Body` is only used in type bounds above.
#[allow(dead_code)]
fn _body_marker(_: Body) {}
