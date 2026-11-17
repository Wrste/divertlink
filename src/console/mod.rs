//! Built-in web console: a single-binary, cross-platform GUI for DivertLink.
//!
//! ```text
//! divertlink console            # http://127.0.0.1:17653
//! divertlink console --open     # also opens the default browser / PWA window
//! ```
//!
//! The console manages *profiles* (a `server.toml` or `agent.toml` plus a friendly
//! name), starts/stops the real binary as child processes, streams their logs,
//! probes gateway TLS connectivity and can scaffold new projects.
//!
//! Inspiration for the UX comes from the excellent open-source AI coding-tool
//! switchers (cc-switch and friends); the console keeps their "profile list +
//! one-click start + live status" model while staying dependency free:
//!
//! * <https://github.com/farion1231/cc-switch>
//! * <https://github.com/cp-yu/cc-switch-web>
//! * <https://github.com/luckjiawei/frpc-desktop>
//! * <https://github.com/koho/frpmgr>

pub mod api;
pub mod assets;
pub mod probe;
pub mod profile;
pub mod supervisor;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::error::{Error, Result};
use crate::util::unix_now;
use profile::ProfileStore;
use supervisor::Supervisor;

/// Default console address (loopback, high port).
pub const DEFAULT_CONSOLE_ADDR: &str = "127.0.0.1:17653";

/// Console startup options.
#[derive(Debug, Clone)]
pub struct ConsoleOptions {
    /// Listen address.
    pub listen: SocketAddr,
    /// Access token; required when `listen` is not loopback.
    pub token: Option<String>,
    /// Open the UI in the default browser after startup.
    pub open_browser: bool,
    /// Override the profile store path (tests / portable mode).
    pub store_path: Option<std::path::PathBuf>,
    /// Start profiles marked `autostart`.
    pub autostart: bool,
}

impl Default for ConsoleOptions {
    fn default() -> Self {
        ConsoleOptions {
            listen: DEFAULT_CONSOLE_ADDR
                .parse()
                .expect("default console address is valid"),
            token: None,
            open_browser: false,
            store_path: None,
            autostart: true,
        }
    }
}

/// A running console.
pub struct ConsoleHandle {
    /// Bound address.
    pub addr: SocketAddr,
    /// Shared state (useful for tests).
    pub state: Arc<api::ConsoleState>,
    shutdown: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

impl ConsoleHandle {
    /// URL to open in a browser.
    pub fn url(&self) -> String {
        let host = if self.addr.ip().is_loopback() {
            "127.0.0.1".to_string()
        } else {
            self.addr.ip().to_string()
        };
        format!("http://{host}:{}/", self.addr.port())
    }

    /// Stop the console (stops all child processes first).
    pub async fn shutdown(self) {
        self.shutdown.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(15), self.task).await;
    }

    /// Signal shutdown without waiting.
    pub fn cancel(&self) {
        self.shutdown.cancel();
    }

    /// Clone of the shutdown token (lets callers wait for API-triggered shutdown).
    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }
}

/// Start the console.
pub async fn run(options: ConsoleOptions) -> Result<ConsoleHandle> {
    if !options.listen.ip().is_loopback() && options.token.is_none() {
        return Err(Error::Config(format!(
            "console may only listen on non-loopback addresses with --token \
             (refusing to expose control UI on {})",
            options.listen
        )));
    }
    let store_path = options
        .store_path
        .clone()
        .unwrap_or_else(profile::default_store_path);
    let store = ProfileStore::open(&store_path)?;
    let shutdown = CancellationToken::new();
    let supervisor = Supervisor::new(store.clone(), shutdown.clone());
    let http = crate::upstream::build_http_client(None, Duration::from_secs(5))?;

    let listener = TcpListener::bind(options.listen).await.map_err(|e| {
        Error::Io(std::io::Error::new(
            e.kind(),
            format!("cannot bind console listener {}: {e}", options.listen),
        ))
    })?;
    let addr = listener.local_addr()?;
    let state = Arc::new(api::ConsoleState {
        store: store.clone(),
        supervisor: supervisor.clone(),
        started_unix: unix_now(),
        listen: addr,
        token: options.token.clone(),
        http,
        shutdown: shutdown.clone(),
    });
    let router = api::router(state.clone());

    let shutdown_accept = shutdown.clone();
    let task = tokio::spawn(async move {
        let serve = axum::serve(listener, router).with_graceful_shutdown(async move {
            shutdown_accept.cancelled().await;
        });
        if let Err(err) = serve.await {
            warn!(error = %err, "console server stopped with an error");
        }
    });

    let handle = ConsoleHandle {
        addr,
        state,
        shutdown: shutdown.clone(),
        task,
    };
    info!(
        addr = %addr,
        url = %handle.url(),
        store = %store_path.display(),
        "divertlink console listening"
    );

    if options.autostart {
        let supervisor = supervisor.clone();
        tokio::spawn(async move { supervisor.start_autostart().await });
    }
    if options.open_browser {
        open_browser(&handle.url());
    }
    Ok(handle)
}

/// Open a URL in the platform browser. Failures are logged, never fatal.
pub fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let command = ("open", vec![url]);
    #[cfg(target_os = "windows")]
    let command = ("cmd", vec!["/C", "start", "", url]);
    #[cfg(all(unix, not(target_os = "macos")))]
    let command = ("xdg-open", vec![url]);

    match std::process::Command::new(command.0)
        .args(command.1)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(mut child) => {
            // Reap the helper so it does not linger as a zombie.
            tokio::spawn(async move {
                let _ = tokio::task::spawn_blocking(move || child.wait()).await;
            });
        }
        Err(err) => warn!(%url, error = %err, "cannot open a browser; open the URL manually"),
    }
}

/// Run the console until Ctrl-C / SIGTERM, then stop children gracefully.
pub async fn run_until_signal(options: ConsoleOptions) -> Result<()> {
    let handle = run(options).await?;
    let url = handle.url();
    {
        use std::io::Write as _;
        let mut stdout = std::io::stdout();
        let _ = writeln!(stdout, "DivertLink 控制台已启动：{url}");
        let _ = writeln!(stdout, "按 Ctrl-C 退出（会停止由控制台启动的进程）。");
    }
    let console_shutdown = handle.shutdown_token();
    tokio::select! {
        _ = crate::signal::shutdown_signal() => info!("console shutdown signal received"),
        _ = console_shutdown.cancelled() => info!("console shutdown requested via API"),
    }
    handle.shutdown().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_options_are_loopback() {
        let options = ConsoleOptions::default();
        assert!(options.listen.ip().is_loopback());
        assert_eq!(options.listen.port(), 17653);
    }

    #[tokio::test]
    async fn refuses_non_loopback_without_token() {
        let options = ConsoleOptions {
            listen: "0.0.0.0:0".parse().unwrap(),
            token: None,
            open_browser: false,
            store_path: None,
            autostart: false,
        };
        assert!(run(options).await.is_err());
    }

    #[tokio::test]
    async fn starts_and_serves_state() {
        let tmp = tempfile::TempDir::new().unwrap();
        let options = ConsoleOptions {
            listen: "127.0.0.1:0".parse().unwrap(),
            token: None,
            open_browser: false,
            store_path: Some(tmp.path().join("profiles.json")),
            autostart: false,
        };
        let handle = run(options).await.unwrap();
        assert!(handle.url().starts_with("http://127.0.0.1:"));
        let client = crate::upstream::build_http_client(None, Duration::from_secs(2)).unwrap();
        let url = format!("{}api/state", handle.url());
        let response = client
            .request(
                axum::http::Request::builder()
                    .uri(url)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let bytes = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["ok"], true);
        assert!(json["data"]["profiles"].as_array().unwrap().is_empty());
        handle.shutdown().await;
    }
}
