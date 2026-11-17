//! Command-line interface: `divertlink server|agent|init|gen-certs|token|check|version`.

use std::io::Write;
use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use tracing::info;

use crate::config::{AgentConfig, ServerConfig};

/// Print a line to stdout, tolerating a closed pipe (`divertlink ... | head`).
///
/// Rust's `println!` panics with "failed printing to stdout: Broken pipe" when the
/// reader goes away, which makes CLI tools unpleasant to pipe. We ignore write
/// errors instead (standard Unix behaviour for `head`/`less` pipelines).
macro_rules! outln {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stdout(), $($arg)*);
    }};
}

use crate::error::{Error, Result};
use crate::telemetry;
use crate::tls::GenCertsOptions;

/// Top-level CLI.
#[derive(Debug, Parser)]
#[command(
    name = "divertlink",
    version,
    about = "Secure reverse tunnel that exposes home-hosted LLM APIs behind a fixed public IP",
    long_about = "DivertLink connects a home-hosted OpenAI/vLLM/Anthropic compatible model server \
to a public gateway with a stable IP. It is protocol transparent, streams SSE without buffering, \
and can be used directly as a new-api/one-api upstream.",
    after_help = "Docs: README.md, docs/quickstart.md\n\
Examples:\n  \
divertlink init --role both --domain llm.example.com\n  \
divertlink server -c server.toml\n  \
divertlink agent -c agent.toml\n  \
divertlink console --open\n  \
divertlink gen-certs --dir certs --domain llm.example.com --client"
)]
pub struct Cli {
    /// Subcommand to run.
    #[command(subcommand)]
    pub command: Command,
}

/// Available subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the public gateway (fixed IP server).
    Server(ServerArgs),
    /// Run the home agent (connects out to the gateway).
    Agent(AgentArgs),
    /// Generate gateway + agent configuration and certificates in one step.
    Init(InitArgs),
    /// Generate a CA, server certificate and optional client certificate.
    GenCerts(GenCertsArgs),
    /// Generate random API keys / agent tokens.
    Token(TokenArgs),
    /// Validate a server or agent configuration file.
    Check(CheckArgs),
    /// Run the built-in web console (cross-platform GUI in a browser/PWA).
    Console(ConsoleArgs),
    /// Print version and build information.
    Version,
}

/// Arguments for `server`.
#[derive(Debug, clap::Args)]
pub struct ServerArgs {
    /// Path to server.toml.
    #[arg(short, long, default_value = "server.toml", env = "DIVERTLINK_CONFIG")]
    pub config: PathBuf,
    /// Validate the configuration and exit without binding sockets.
    #[arg(long)]
    pub check: bool,
    /// Reduce log output to warnings and errors (overrides `[log].level`).
    #[arg(long)]
    pub quiet: bool,
}

/// Arguments for `agent`.
#[derive(Debug, clap::Args)]
pub struct AgentArgs {
    /// Path to agent.toml.
    #[arg(short, long, default_value = "agent.toml", env = "DIVERTLINK_CONFIG")]
    pub config: PathBuf,
    /// Validate the configuration and exit without connecting.
    #[arg(long)]
    pub check: bool,
    /// Connect once and exit when the session ends (no automatic reconnect).
    #[arg(long)]
    pub once: bool,
    /// Reduce log output to warnings and errors (overrides `[log].level`).
    #[arg(long)]
    pub quiet: bool,
}

/// Role for `init`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Role {
    /// Only write server.toml.
    Server,
    /// Only write agent.toml.
    Agent,
    /// Write both ends plus certificates.
    Both,
}

/// Arguments for `init`.
#[derive(Debug, clap::Args)]
pub struct InitArgs {
    /// Which side(s) of the tunnel to configure.
    #[arg(long, value_enum, default_value_t = Role::Both)]
    pub role: Role,
    /// Output directory (configs and certs/ inside it).
    #[arg(long, default_value = ".")]
    pub dir: PathBuf,
    /// Public domain or IP of the gateway (used in certificates and agent config).
    #[arg(long, default_value = "localhost")]
    pub domain: String,
    /// Generate mutual TLS material as well.
    #[arg(long)]
    pub mtls: bool,
    /// Overwrite existing files.
    #[arg(long)]
    pub force: bool,
}

/// Arguments for `gen-certs`.
#[derive(Debug, clap::Args)]
pub struct GenCertsArgs {
    /// Output directory.
    #[arg(long, default_value = "certs")]
    pub dir: PathBuf,
    /// DNS name / IP the server certificate must be valid for.
    #[arg(long, default_value = "localhost")]
    pub domain: String,
    /// Generate a single self-signed certificate instead of a CA + leaf pair.
    #[arg(long)]
    pub self_signed: bool,
    /// Also generate a client certificate for mutual TLS.
    #[arg(long)]
    pub client: bool,
    /// Overwrite existing files.
    #[arg(long)]
    pub force: bool,
}

/// Arguments for `token`.
#[derive(Debug, clap::Args)]
pub struct TokenArgs {
    /// Prefix for generated tokens (e.g. `dlk_` for API keys, `agt_` for agent tokens).
    #[arg(long, default_value = "dlk_")]
    pub prefix: String,
    /// How many tokens to generate.
    #[arg(long, default_value_t = 1)]
    pub count: u32,
    /// Number of random bytes per token (base64url encoded afterwards).
    #[arg(long, default_value_t = 24)]
    pub bytes: usize,
}

/// Arguments for `console`.
#[derive(Debug, clap::Args)]
pub struct ConsoleArgs {
    /// Listen address (default 127.0.0.1:17653). Use 0 for a random free port.
    #[arg(long, default_value = "127.0.0.1:17653")]
    pub listen: String,
    /// Access token required for the console API (mandatory for non-loopback).
    #[arg(long, env = "DIVERTLINK_CONSOLE_TOKEN")]
    pub token: Option<String>,
    /// Open the UI in the default browser after startup.
    #[arg(long)]
    pub open: bool,
    /// Override the profile store path.
    #[arg(long)]
    pub store: Option<std::path::PathBuf>,
    /// Do not auto-start profiles marked with autostart.
    #[arg(long)]
    pub no_autostart: bool,
}

/// Arguments for `check`.
#[derive(Debug, clap::Args)]
pub struct CheckArgs {
    /// Configuration file to validate.
    #[arg(short, long, default_value = "server.toml")]
    pub config: PathBuf,
    /// Treat security/robustness warnings as errors.
    #[arg(long)]
    pub strict: bool,
}

/// Entry point used by `main`.
pub fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Version => {
            print_version();
            Ok(())
        }
        Command::Server(args) => cmd_server(args),
        Command::Agent(args) => cmd_agent(args),
        Command::Init(args) => cmd_init(args),
        Command::GenCerts(args) => cmd_gen_certs(args),
        Command::Token(args) => {
            if args.count == 0 || args.count > 1000 {
                return Err(Error::Config("--count must be within 1..=1000".into()));
            }
            if args.bytes < 8 || args.bytes > 128 {
                return Err(Error::Config("--bytes must be within 8..=128".into()));
            }
            let mut out = std::io::stdout().lock();
            for _ in 0..args.count {
                let _ = writeln!(
                    out,
                    "{}",
                    crate::util::random_token(&args.prefix, args.bytes)
                );
            }
            Ok(())
        }
        Command::Console(args) => cmd_console(args),
        Command::Check(args) => cmd_check(args),
    }
}

fn cmd_console(args: ConsoleArgs) -> Result<()> {
    let listen = args
        .listen
        .parse::<std::net::SocketAddr>()
        .map_err(|e| Error::Config(format!("invalid --listen `{}`: {e}", args.listen)))?;
    init_logging("info");
    let runtime = build_runtime()?;
    runtime.block_on(async move {
        let options = crate::console::ConsoleOptions {
            listen,
            token: args.token,
            open_browser: args.open,
            store_path: args.store,
            autostart: !args.no_autostart,
        };
        crate::console::run_until_signal(options).await
    })
}

fn print_version() {
    outln!("divertlink {}", env!("CARGO_PKG_VERSION"));
    outln!("protocol: {}", crate::protocol::PROTOCOL_VERSION);
    outln!("os: {} {}", std::env::consts::OS, std::env::consts::ARCH);
    outln!("tls: rustls (ring), TLS 1.3");
    outln!("features: yamux multiplexing, SSE streaming, OpenAI/Anthropic/vLLM");
    outln!("console: built-in local web UI / PWA (divertlink console --open)");
}

fn init_logging(level: &str) {
    let settings = crate::config::LogSettings {
        level: level.to_string(),
        format: crate::config::LogFormat::Text,
    };
    let _ = telemetry::init(&settings);
}

fn cmd_server(args: ServerArgs) -> Result<()> {
    let config = ServerConfig::load(&args.config)?;
    init_logging(if args.quiet {
        "warn"
    } else {
        &config.log.level
    });
    if args.check {
        outln!("server configuration OK: {}", args.config.display());
        outln!(
            "  tunnels: {} | api keys: {} | static routes: {}",
            config.tunnels.len(),
            config.api_keys.len(),
            config.static_routes.len()
        );
        return Ok(());
    }
    let runtime = build_runtime()?;
    runtime.block_on(async move {
        info!(
            version = env!("CARGO_PKG_VERSION"),
            config = %args.config.display(),
            "starting divertlink gateway"
        );
        crate::server::run_until_signal(config).await
    })
}

fn cmd_agent(args: AgentArgs) -> Result<()> {
    let config = AgentConfig::load(&args.config)?;
    init_logging(if args.quiet {
        "warn"
    } else {
        &config.log.level
    });
    if args.check {
        outln!("agent configuration OK: {}", args.config.display());
        outln!(
            "  tunnel: {} | gateway: {} | target: {}",
            config.agent.id,
            config.agent.server,
            config.target.url
        );
        return Ok(());
    }
    let runtime = build_runtime()?;
    runtime.block_on(async move {
        info!(
            version = env!("CARGO_PKG_VERSION"),
            config = %args.config.display(),
            tunnel = %config.agent.id,
            "starting divertlink agent"
        );
        if args.once {
            crate::agent::run_once(config).await
        } else {
            crate::agent::run_until_signal(config).await
        }
    })
}

fn build_runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("divertlink")
        .build()
        .map_err(|e| Error::Internal(format!("cannot start the async runtime: {e}")))
}

fn cmd_gen_certs(args: GenCertsArgs) -> Result<()> {
    let report = crate::tls::generate_certs(&GenCertsOptions {
        dir: args.dir.clone(),
        domain: args.domain.clone(),
        client_cert: args.client && !args.self_signed,
        self_signed: args.self_signed,
        force: args.force,
    })?;
    outln!("certificates written to {}", args.dir.display());
    outln!("  ca       : {}", report.ca_cert.display());
    outln!("  server   : {}", report.server_cert.display());
    outln!("  key      : {}", report.server_key.display());
    if let Some(client) = &report.client_cert {
        outln!("  client   : {}", client.display());
    }
    outln!();
    outln!("server certificate SPKI pin (for agent tls.pin):");
    outln!("  {}", report.server_pin);
    outln!();
    if args.self_signed {
        outln!(
            "Self-signed mode: point the agent's tls.ca at {}",
            report.ca_cert.display()
        );
    } else {
        outln!(
            "Distribute {} to agents and set agent tls.ca to it.",
            report.ca_cert.display()
        );
    }
    Ok(())
}

fn cmd_init(args: InitArgs) -> Result<()> {
    let role = match args.role {
        Role::Server => crate::setup::InitRole::Server,
        Role::Agent => crate::setup::InitRole::Agent,
        Role::Both => crate::setup::InitRole::Both,
    };
    let report = crate::setup::init_project(&crate::setup::InitOptions {
        dir: args.dir.clone(),
        domain: args.domain.clone(),
        role,
        mtls: args.mtls,
        force: args.force,
    })?;

    if let Some(path) = &report.server_toml {
        outln!("wrote {}", path.display());
    }
    if let Some(path) = &report.agent_toml {
        outln!("wrote {}", path.display());
    }

    outln!();
    outln!("next steps:");
    if let Some(_path) = &report.server_toml {
        outln!("  1. copy certs/ and server.toml to the gateway host");
        outln!("     divertlink server -c server.toml");
        outln!("  2. API key for new-api / SDK clients: {}", report.api_key);
    }
    if let Some(_path) = &report.agent_toml {
        let domain = args.domain.trim();
        outln!("  3. copy agent.toml (+ certs/ca.crt) to the home host");
        outln!("     divertlink agent -c agent.toml");
        outln!(
            "  4. test: curl -N https://{domain}/home/v1/models -H 'Authorization: Bearer {}'",
            report.api_key
        );
    }
    if role == crate::setup::InitRole::Agent {
        outln!();
        outln!("   note: --role agent does not generate certificates; copy certs/ca.crt from the gateway");
    }
    if args.mtls {
        outln!();
        outln!("   mutual TLS is generated but not enabled automatically:");
        outln!("   - server.toml: set tls.client_ca = \"certs/ca.crt\" and use a dedicated tunnel_listen");
        outln!("     (tunnel_listen must differ from ingress_listen when mTLS is enabled)");
        if role.has_server() {
            outln!("   - agent.toml already points at certs/client.crt and certs/client.key");
        }
    }
    if role.has_server() {
        outln!();
        outln!("   tip: run `divertlink console` for a graphical dashboard of both ends");
    }
    Ok(())
}

#[cfg(test)]
#[allow(dead_code)]
fn write_file(path: &std::path::Path, contents: &str, force: bool) -> Result<()> {
    if path.exists() && !force {
        return Err(Error::Config(format!(
            "{} already exists; pass --force to overwrite",
            path.display()
        )));
    }
    std::fs::write(path, contents)
        .map_err(|e| Error::Config(format!("cannot write {}: {e}", path.display())))
}

fn cmd_check(args: CheckArgs) -> Result<()> {
    let config = args.config.clone();
    let text = std::fs::read_to_string(&config)
        .map_err(|e| Error::Config(format!("cannot read {}: {e}", config.display())))?;

    let base = config.parent().unwrap_or_else(|| std::path::Path::new("."));
    let warnings = match crate::config::ServerConfig::from_toml_str(&text) {
        Ok(mut parsed) => {
            parsed.resolve_paths(base);
            outln!("server configuration OK: {}", config.display());
            outln!(
                "  tunnels: {} | api keys: {} | static routes: {}",
                parsed.tunnels.len(),
                parsed.api_keys.len(),
                parsed.static_routes.len()
            );
            parsed.lint()
        }
        Err(server_err) => match crate::config::AgentConfig::from_toml_str(&text) {
            Ok(mut parsed) => {
                parsed.resolve_paths(base);
                outln!("agent configuration OK: {}", config.display());
                outln!(
                    "  tunnel: {} | gateway: {} | target: {}",
                    parsed.agent.id, parsed.agent.server, parsed.target.url
                );
                parsed.lint()
            }
            Err(agent_err) => {
                return Err(Error::Config(format!(
                    "{}: not a valid server config ({server_err}); not a valid agent config ({agent_err})",
                    config.display()
                )))
            }
        },
    };
    if warnings.is_empty() {
        outln!("  no security warnings");
    } else {
        for warning in &warnings {
            eprintln!("warning: {warning}");
        }
        outln!(
            "  {} warning(s); run with --strict to fail on them",
            warnings.len()
        );
    }
    if args.strict && !warnings.is_empty() {
        return Err(Error::Config(format!(
            "{} warning(s) treated as errors because of --strict",
            warnings.len()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parses_server() {
        let cli = Cli::try_parse_from(["divertlink", "server", "-c", "x.toml"]).unwrap();
        match cli.command {
            Command::Server(args) => assert_eq!(args.config, PathBuf::from("x.toml")),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn cli_parses_init_and_gen_certs() {
        let cli = Cli::try_parse_from([
            "divertlink",
            "init",
            "--role",
            "both",
            "--domain",
            "llm.example.com",
            "--dir",
            "/tmp/x",
        ])
        .unwrap();
        match cli.command {
            Command::Init(args) => {
                assert_eq!(args.role, Role::Both);
                assert_eq!(args.domain, "llm.example.com");
            }
            other => panic!("unexpected {other:?}"),
        }
        let cli = Cli::try_parse_from(["divertlink", "gen-certs", "--client", "--self-signed"]);
        // self-signed + client is rejected later with a clear error, but parsing works.
        assert!(cli.is_ok());
    }

    #[test]
    fn cli_parses_token_and_check() {
        let cli = Cli::try_parse_from(["divertlink", "token", "--prefix", "agt_", "--count", "3"])
            .unwrap();
        match cli.command {
            Command::Token(args) => {
                assert_eq!(args.prefix, "agt_");
                assert_eq!(args.count, 3);
            }
            other => panic!("unexpected {other:?}"),
        }
        let cli = Cli::try_parse_from(["divertlink", "check", "-c", "agent.toml"]).unwrap();
        assert!(matches!(cli.command, Command::Check(_)));
    }

    #[test]
    fn writes_file_once() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("a.toml");
        write_file(&path, "x", false).unwrap();
        assert!(write_file(&path, "y", false).is_err());
        write_file(&path, "y", true).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "y");
    }
}
