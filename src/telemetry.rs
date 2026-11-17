//! Logging initialization shared by both binaries.

use std::io::IsTerminal;

use tracing_subscriber::EnvFilter;

use crate::config::{LogFormat, LogSettings};
use crate::error::Result;

/// Initialize the global tracing subscriber.
///
/// `RUST_LOG` overrides the configured level when present. Calling this more
/// than once is harmless (the second call is ignored), which keeps tests simple.
pub fn init(settings: &LogSettings) -> Result<()> {
    let filter = match EnvFilter::try_from_default_env() {
        Ok(filter) => filter,
        Err(_) => EnvFilter::try_new(&settings.level).map_err(|e| {
            crate::error::Error::Config(format!("invalid log level `{}`: {e}", settings.level))
        })?,
    };

    match settings.format {
        LogFormat::Text => {
            let ansi = std::io::stdout().is_terminal();
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_target(true)
                .with_ansi(ansi)
                .try_init();
        }
        LogFormat::Json => {
            let _ = tracing_subscriber::fmt()
                .json()
                .with_env_filter(filter)
                .with_target(true)
                .with_current_span(false)
                .with_span_list(false)
                .try_init();
        }
    }
    Ok(())
}
