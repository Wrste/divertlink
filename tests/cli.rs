//! CLI integration tests: exercise the real binary for `init`, `gen-certs`,
//! `token` and `check`.

use std::path::Path;
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_divertlink")
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(bin())
        .args(args)
        .output()
        .expect("failed to run divertlink")
}

fn run_ok(args: &[&str]) -> String {
    let output = run(args);
    assert!(
        output.status.success(),
        "command {args:?} failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

#[test]
fn version_reports_protocol() {
    let out = run_ok(&["version"]);
    assert!(out.contains("divertlink "), "{out}");
    assert!(out.contains("protocol: 1"), "{out}");
    assert!(out.contains("TLS 1.3"), "{out}");
}

#[test]
fn token_generates_unique_prefixed_secrets() {
    let out = run_ok(&["token", "--prefix", "agt_", "--count", "3"]);
    let tokens: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(tokens.len(), 3);
    assert!(tokens.iter().all(|t| t.starts_with("agt_")));
    assert_ne!(tokens[0], tokens[1]);
    // Reject bad counts.
    assert!(!run(&["token", "--count", "0"]).status.success());
}

#[test]
fn init_generates_working_configs_and_certs() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().join("deploy");
    let dir_s = dir.to_str().unwrap();
    let out = run_ok(&[
        "init",
        "--role",
        "both",
        "--dir",
        dir_s,
        "--domain",
        "localhost",
        "--mtls",
        "--force",
    ]);
    assert!(out.contains("next steps"), "{out}");

    for file in [
        "server.toml",
        "agent.toml",
        "certs/ca.crt",
        "certs/server.crt",
        "certs/server.key",
        "certs/client.crt",
        "certs/client.key",
    ] {
        assert!(
            dir.join(file).exists(),
            "init did not create {file}: {}",
            dir.display()
        );
    }

    // The generated configs must validate with the same binary.
    let server = run_ok(&["check", "-c", dir.join("server.toml").to_str().unwrap()]);
    assert!(server.contains("server configuration OK"), "{server}");
    let agent = run_ok(&["check", "-c", dir.join("agent.toml").to_str().unwrap()]);
    assert!(agent.contains("agent configuration OK"), "{agent}");

    // The generated agent config points at the generated CA and carries a pin.
    let agent_toml = std::fs::read_to_string(dir.join("agent.toml")).unwrap();
    assert!(agent_toml.contains("ca = \"certs/ca.crt\""), "{agent_toml}");
    assert!(agent_toml.contains("pin = \""), "{agent_toml}");
    assert!(agent_toml.contains("client_cert"), "{agent_toml}");

    // Key material must not be world readable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.join("certs/server.key"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o077,
            0,
            "private key must not be group/world readable"
        );
    }

    // Without --force the second run must refuse to clobber secrets.
    let again = run(&[
        "init",
        "--role",
        "both",
        "--dir",
        dir_s,
        "--domain",
        "localhost",
    ]);
    assert!(
        !again.status.success(),
        "init must not overwrite without --force"
    );
}

#[test]
fn init_server_only_does_not_write_agent() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().join("srv");
    run_ok(&[
        "init",
        "--role",
        "server",
        "--dir",
        dir.to_str().unwrap(),
        "--domain",
        "127.0.0.1",
        "--force",
    ]);
    assert!(dir.join("server.toml").exists());
    assert!(!dir.join("agent.toml").exists());
}

#[test]
fn gen_certs_self_signed_and_force() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().join("certs");
    let out = run_ok(&[
        "gen-certs",
        "--dir",
        dir.to_str().unwrap(),
        "--domain",
        "llm.example.com",
        "--self-signed",
    ]);
    assert!(out.contains("SPKI pin"), "{out}");
    assert!(dir.join("server.crt").exists());
    assert!(dir.join("server.key").exists());
    // A second invocation must fail without --force.
    assert!(!run(&[
        "gen-certs",
        "--dir",
        dir.to_str().unwrap(),
        "--domain",
        "llm.example.com",
        "--self-signed",
    ])
    .status
    .success());
    // ... and succeed with --force.
    run_ok(&[
        "gen-certs",
        "--dir",
        dir.to_str().unwrap(),
        "--domain",
        "llm.example.com",
        "--self-signed",
        "--force",
    ]);
}

#[test]
fn check_rejects_bad_configs() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("bad.toml");
    std::fs::write(
        &path,
        r#"
[agent]
id = "home"
server = "gateway.example.com:443"
token = "x"
heartbeat = "10s"
heartbeat_timeout = "5s"
"#,
    )
    .unwrap();
    let output = run(&["check", "-c", path.to_str().unwrap()]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("heartbeat_timeout"), "{stderr}");

    // Unknown fields are rejected with a helpful message.
    std::fs::write(
        &path,
        r#"
[agent]
id = "home"
server = "gateway.example.com:443"
token = "x"
bogus_field = 1
"#,
    )
    .unwrap();
    let output = run(&["check", "-c", path.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .to_lowercase()
            .contains("unknown"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn missing_config_is_reported() {
    let output = run(&["agent", "-c", "/nonexistent/agent.toml", "--check"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cannot read"), "{stderr}");
}

#[test]
fn help_mentions_subcommands() {
    let out = run_ok(&["--help"]);
    for cmd in ["server", "agent", "init", "gen-certs", "token", "check"] {
        assert!(out.contains(cmd), "help is missing {cmd}: {out}");
    }
    let _ = Path::new(bin());
}
