//! DivertLink library: a secure reverse tunnel that exposes LLM servers running
//! on home broadband behind a public, fixed-IP gateway.
//!
//! The crate is split into a reusable library plus thin `server` / `agent`
//! binaries driven by [`cli`]. See `docs/DESIGN.md` for the architecture.

pub mod agent;
pub mod auth;
pub mod cli;
pub mod config;
pub mod console;
pub mod error;
pub mod httpfwd;
pub mod metrics;
pub mod net;
pub mod protocol;
pub mod ratelimit;
pub mod registry;
pub mod server;
pub mod setup;
pub mod signal;
pub mod telemetry;
pub mod tls;
pub mod upstream;
pub mod util;

pub use error::{Error, Result};
