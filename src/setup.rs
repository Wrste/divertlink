//! One-shot project scaffolding shared by the CLI (`divertlink init`) and the
//! built-in web console.

use std::path::{Path, PathBuf};

use crate::config::{AgentConfig, ServerConfig};
use crate::error::{Error, Result};
use crate::tls::{generate_certs, CertReport, GenCertsOptions};

/// Which side(s) of the tunnel to scaffold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitRole {
    /// Gateway only (server.toml + certificates).
    Server,
    /// Home agent only (agent.toml; certificates must be copied from the gateway).
    Agent,
    /// Both ends on one machine (development / all-in-one setups).
    Both,
}

impl InitRole {
    /// Parse from CLI/UI strings.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "server" | "gateway" | "srv" => Some(Self::Server),
            "agent" | "client" | "home" => Some(Self::Agent),
            "both" | "all" => Some(Self::Both),
            _ => None,
        }
    }

    /// Whether the gateway side is generated.
    pub fn has_server(&self) -> bool {
        matches!(self, Self::Server | Self::Both)
    }

    /// Whether the home side is generated.
    pub fn has_agent(&self) -> bool {
        matches!(self, Self::Agent | Self::Both)
    }
}

/// Options for [`init_project`].
#[derive(Debug, Clone)]
pub struct InitOptions {
    /// Output directory (created if missing); certificates go to `<dir>/certs`.
    pub dir: PathBuf,
    /// Gateway domain or IP used in certificate SANs and the agent endpoint.
    pub domain: String,
    /// Which side(s) to generate.
    pub role: InitRole,
    /// Also generate a client certificate for mutual TLS.
    pub mtls: bool,
    /// Overwrite existing files.
    pub force: bool,
}

/// Result of scaffolding a project.
#[derive(Debug, Clone)]
pub struct InitReport {
    /// Written server.toml, if the gateway side was generated.
    pub server_toml: Option<PathBuf>,
    /// Written agent.toml, if the home side was generated.
    pub agent_toml: Option<PathBuf>,
    /// Generated certificates (gateway side only).
    pub certs: Option<CertReport>,
    /// Random API key written into server.toml.
    pub api_key: String,
    /// Random agent token shared by both configs.
    pub agent_token: String,
}

/// Generate configurations and certificates in `opts.dir`.
///
/// The generated files are immediately usable:
/// `divertlink server -c <dir>/server.toml` and
/// `divertlink agent -c <dir>/agent.toml`.
pub fn init_project(opts: &InitOptions) -> Result<InitReport> {
    let domain = opts.domain.trim().to_string();
    if domain.is_empty() {
        return Err(Error::Config("domain must not be empty".into()));
    }
    std::fs::create_dir_all(&opts.dir)
        .map_err(|e| Error::Config(format!("cannot create {}: {e}", opts.dir.display())))?;
    let certs_dir = opts.dir.join("certs");

    let report = if opts.role.has_server() {
        Some(generate_certs(&GenCertsOptions {
            dir: certs_dir.clone(),
            domain: domain.clone(),
            client_cert: opts.mtls,
            self_signed: false,
            force: opts.force,
        })?)
    } else {
        None
    };

    let api_key = crate::util::random_token("dlk_", 24);
    let agent_token = crate::util::random_token("agt_", 24);

    let mut server_toml = None;
    if opts.role.has_server() {
        let mut text = ServerConfig::example().to_string();
        text = text.replace("dlk_replace_me_with_a_random_token", &api_key);
        text = text.replace("agt_replace_me_with_a_random_token", &agent_token);
        let path = opts.dir.join("server.toml");
        write_file(&path, &text, opts.force)?;
        server_toml = Some(path);
    }

    let mut agent_toml = None;
    if opts.role.has_agent() {
        let mut text = AgentConfig::example().to_string();
        text = text.replace("agt_replace_me_with_a_random_token", &agent_token);
        text = text.replace(
            "server = \"gateway.example.com:443\"",
            &format!("server = \"{domain}:443\""),
        );
        if opts.role == InitRole::Both {
            text = text.replace("# ca = \"certs/ca.crt\"", "ca = \"certs/ca.crt\"");
        }
        if let Some(report) = &report {
            text = text.replace("# pin = \"\"", &format!("pin = \"{}\"", report.server_pin));
            if report.client_cert.is_some() && report.client_key.is_some() {
                text = text.replace(
                    "# client_cert = \"certs/client.crt\"",
                    "client_cert = \"certs/client.crt\"",
                );
                text = text.replace(
                    "# client_key = \"certs/client.key\"",
                    "client_key = \"certs/client.key\"",
                );
            }
        }
        let path = opts.dir.join("agent.toml");
        write_file(&path, &text, opts.force)?;
        agent_toml = Some(path);
    }

    Ok(InitReport {
        server_toml,
        agent_toml,
        certs: report,
        api_key,
        agent_token,
    })
}

fn write_file(path: &Path, contents: &str, force: bool) -> Result<()> {
    if path.exists() && !force {
        return Err(Error::Config(format!(
            "{} already exists; use force/--force to overwrite",
            path.display()
        )));
    }
    std::fs::write(path, contents)
        .map_err(|e| Error::Config(format!("cannot write {}: {e}", path.display())))
}
