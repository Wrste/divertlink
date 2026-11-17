//! Command-line interface: `divertlink server|agent|init|gen-certs|token|check|version`.

use std::io::Write;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand, ValueEnum};
use tracing::info;

use crate::config::{AgentConfig, ServerConfig};
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

/// Arguments for `check`.
#[derive(Debug, clap::Args)]
pub struct CheckArgs {
    /// Configuration file to validate.
    #[arg(short, long, default_value = "server.toml")]
    pub config: PathBuf,
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
        Command::Check(args) => cmd_check(args),
    }
}

fn print_version() {
    println!("divertlink {}", env!("CARGO_PKG_VERSION"));
    println!("protocol: {}", crate::protocol::PROTOCOL_VERSION);
    println!("os: {} {}", std::env::consts::OS, std::env::consts::ARCH);
    println!("tls: rustls (ring), TLS 1.3");
    println!("features: yamux multiplexing, SSE streaming, OpenAI/Anthropic/vLLM");
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
        println!("server configuration OK: {}", args.config.display());
        println!(
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
        println!("agent configuration OK: {}", args.config.display());
        println!(
            "  tunnel: {} | gateway: {} | target: {}",
            config.agent.id, config.agent.server, config.target.url
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
    println!("certificates written to {}", args.dir.display());
    println!("  ca       : {}", report.ca_cert.display());
    println!("  server   : {}", report.server_cert.display());
    println!("  key      : {}", report.server_key.display());
    if let Some(client) = &report.client_cert {
        println!("  client   : {}", client.display());
    }
    println!();
    println!("server certificate SPKI pin (for agent tls.pin):");
    println!("  {}", report.server_pin);
    println!();
    if args.self_signed {
        println!(
            "Self-signed mode: point the agent's tls.ca at {}",
            report.ca_cert.display()
        );
    } else {
        println!(
            "Distribute {} to agents and set agent tls.ca to it.",
            report.ca_cert.display()
        );
    }
    Ok(())
}

fn cmd_init(args: InitArgs) -> Result<()> {
    std::fs::create_dir_all(&args.dir)
        .map_err(|e| Error::Config(format!("cannot create {}: {e}", args.dir.display())))?;
    let domain = args.domain.trim().to_string();
    if domain.is_empty() {
        return Err(Error::Config("--domain must not be empty".into()));
    }
    let certs_dir = args.dir.join("certs");
    let report = if matches!(args.role, Role::Server | Role::Both) {
        Some(crate::tls::generate_certs(&GenCertsOptions {
            dir: certs_dir.clone(),
            domain: domain.clone(),
            client_cert: args.mtls,
            self_signed: false,
            force: args.force,
        })?)
    } else {
        None
    };

    let api_key = crate::util::random_token("dlk_", 24);
    let agent_token = crate::util::random_token("agt_", 24);

    if matches!(args.role, Role::Server | Role::Both) {
        let mut text = ServerConfig::example().to_string();
        text = text.replace("dlk_replace_me_with_a_random_token", &api_key);
        text = text.replace("agt_replace_me_with_a_random_token", &agent_token);
        write_file(&args.dir.join("server.toml"), &text, args.force)?;
        println!("wrote {}", args.dir.join("server.toml").display());
    }
    if matches!(args.role, Role::Agent | Role::Both) {
        let mut text = AgentConfig::example().to_string();
        text = text.replace("agt_replace_me_with_a_random_token", &agent_token);
        text = text.replace(
            "server = \"gateway.example.com:443\"",
            &format!("server = \"{domain}:443\""),
        );
        if matches!(args.role, Role::Both) {
            text = text.replace("# ca = \"certs/ca.crt\"", "ca = \"certs/ca.crt\"");
        }
        if let Some(report) = &report {
            text = text.replace("# pin = \"\"", &format!("pin = \"{}\"", report.server_pin));
            if let (Some(cert), Some(key)) = (&report.client_cert, &report.client_key) {
                text = text.replace(
                    "# client_cert = \"certs/client.crt\"",
                    &format!("client_cert = \"{}\"", cert.display()),
                );
                text = text.replace(
                    "# client_key = \"certs/client.key\"",
                    &format!("client_key = \"{}\"", key.display()),
                );
            }
        }
        write_file(&args.dir.join("agent.toml"), &text, args.force)?;
        println!("wrote {}", args.dir.join("agent.toml").display());
    }

    println!();
    println!("next steps:");
    if matches!(args.role, Role::Server | Role::Both) {
        println!("  1. copy certs/ and server.toml to the gateway host");
        println!("     divertlink server -c server.toml");
        println!("  2. API key for new-api / SDK clients: {api_key}");
    }
    if matches!(args.role, Role::Agent | Role::Both) {
        println!("  3. copy agent.toml (+ certs/ca.crt) to the home host");
        println!("     divertlink agent -c agent.toml");
        println!("  4. test: curl -N https://{domain}/home/v1/models -H 'Authorization: Bearer {api_key}'");
    }
    if matches!(args.role, Role::Agent) {
        println!();
        println!("   note: --role agent does not generate certificates; copy certs/ca.crt from the gateway");
    }
    if args.mtls {
        println!();
        println!("   mutual TLS is generated but not enabled automatically:");
        println!("   - server.toml: set tls.client_ca = \"certs/ca.crt\" and use a dedicated tunnel_listen");
        println!("     (tunnel_listen must differ from ingress_listen when mTLS is enabled)");
        if matches!(args.role, Role::Server | Role::Both) {
            println!("   - agent.toml already points at certs/client.crt and certs/client.key");
        }
    }
    Ok(())
}

fn write_file(path: &Path, contents: &str, force: bool) -> Result<()> {
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
    let text = std::fs::read_to_string(&args.config)
        .map_err(|e| Error::Config(format!("cannot read {}: {e}", args.config.display())))?;
    if text.contains("[agent]") && !text.contains("[server]") {
        let config = AgentConfig::from_toml_str(&text)?;
        println!("agent configuration OK: {}", args.config.display());
        println!(
            "  tunnel: {} | gateway: {} | target: {}",
            config.agent.id, config.agent.server, config.target.url
        );
    } else {
        let config = ServerConfig::from_toml_str(&text)?;
        println!("server configuration OK: {}", args.config.display());
        println!(
            "  tunnels: {} | api keys: {} | static routes: {}",
            config.tunnels.len(),
            config.api_keys.len(),
            config.static_routes.len()
        );
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
