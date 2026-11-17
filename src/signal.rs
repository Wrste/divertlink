//! Cross-platform shutdown signals.
//!
//! Servers must treat SIGTERM (systemd, Docker, the console supervisor) the same
//! way as Ctrl-C so that in-flight requests get a chance to finish and children
//! are reaped.

/// Wait for Ctrl-C (SIGINT) or, on Unix, SIGTERM.
pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut terminate = match signal(SignalKind::terminate()) {
            Ok(signal) => signal,
            Err(err) => {
                tracing::warn!(error = %err, "cannot install SIGTERM handler; only Ctrl-C will stop the process");
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        let mut interrupt = match signal(SignalKind::interrupt()) {
            Ok(signal) => signal,
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = terminate.recv() => tracing::info!("SIGTERM received"),
            _ = interrupt.recv() => tracing::info!("SIGINT received"),
            _ = tokio::signal::ctrl_c() => tracing::info!("Ctrl-C received"),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn is_callable() {
        // The future is only meaningful with a real signal; just assert it type
        // checks and can be constructed twice.
        let _a = super::shutdown_signal();
        let _b = super::shutdown_signal();
    }
}
