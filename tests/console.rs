//! Integration tests for the built-in web console (`divertlink console`).
//!
//! These tests spawn the real binary, drive its REST API over loopback HTTP and
//! verify the UI assets, security guards, project generator and process
//! supervision.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_divertlink")
}

struct ConsoleProc {
    child: Child,
    base: String,
    token: Option<String>,
}

impl ConsoleProc {
    fn start(extra_args: &[&str]) -> Self {
        Self::start_with_token(extra_args, None)
    }

    fn start_with_token(extra_args: &[&str], token: Option<&str>) -> Self {
        let mut command = Command::new(bin());
        command
            .arg("console")
            .arg("--listen")
            .arg("127.0.0.1:0")
            .arg("--no-autostart")
            .args(extra_args);
        if let Some(token) = token {
            command.arg("--token").arg(token);
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("spawn console");
        let stdout = child.stdout.take().expect("stdout");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(|l| l.ok()) {
                if let Some(url) = extract_url(&line) {
                    let _ = tx.send(url);
                }
            }
        });
        // Drain stderr so the child never blocks on a full pipe.
        if let Some(stderr) = child.stderr.take() {
            std::thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for _ in reader.lines() {}
            });
        }
        let base = rx
            .recv_timeout(Duration::from_secs(20))
            .expect("console did not print its URL");
        let proc = ConsoleProc {
            child,
            base,
            token: token.map(str::to_string),
        };
        proc.wait_ready();
        proc
    }

    fn wait_ready(&self) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if http_get(&format!("{}api/healthz", self.base)).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("console API never became ready at {}", self.base);
    }

    fn stop(mut self) {
        let headers: Vec<(&str, &str)> = match &self.token {
            Some(token) => vec![
                ("x-divertlink-console", "1"),
                ("x-console-token", token.as_str()),
            ],
            None => vec![("x-divertlink-console", "1")],
        };
        let _ = http_request(
            "POST",
            &format!("{}api/shutdown", self.base),
            &headers,
            Some("{}"),
        );
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        panic!("console did not shut down");
    }
}

impl Drop for ConsoleProc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn extract_url(line: &str) -> Option<String> {
    let start = line.find("http://")?;
    let rest = &line[start..];
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let mut url = rest[..end].to_string();
    if !url.ends_with('/') {
        url.push('/');
    }
    Some(url)
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

/// Minimal HTTP/1.1 client over a fresh connection (avoids external deps).
fn http_request(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
) -> Result<(u16, String, String), String> {
    let without_scheme = url
        .strip_prefix("http://")
        .ok_or("only http:// supported")?;
    let (authority, path) = match without_scheme.find('/') {
        Some(idx) => (&without_scheme[..idx], &without_scheme[idx..]),
        None => (without_scheme, "/"),
    };
    let mut stream = TcpStream::connect(authority).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(|e| e.to_string())?;
    let mut request =
        format!("{method} {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    let payload = body.unwrap_or("");
    if body.is_some() {
        request.push_str(&format!("Content-Length: {}\r\n", payload.len()));
        request.push_str("Content-Type: application/json\r\n");
    }
    request.push_str("\r\n");
    request.push_str(payload);
    stream
        .write_all(request.as_bytes())
        .map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .map_err(|e| e.to_string())?;
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("bad status line: {status_line}"))?;
    let mut content_length = None;
    let mut chunked = false;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).map_err(|e| e.to_string())?;
        if n == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        let lower = line.to_ascii_lowercase();
        if let Some(value) = lower.strip_prefix("content-length:") {
            content_length = value.trim().parse::<usize>().ok();
        }
        if lower.starts_with("transfer-encoding:") && lower.contains("chunked") {
            chunked = true;
        }
    }
    let mut raw = Vec::new();
    if chunked {
        loop {
            let mut size_line = String::new();
            reader
                .read_line(&mut size_line)
                .map_err(|e| e.to_string())?;
            let size = usize::from_str_radix(size_line.trim(), 16).map_err(|e| e.to_string())?;
            if size == 0 {
                let mut trailer = String::new();
                let _ = reader.read_line(&mut trailer);
                break;
            }
            let mut chunk = vec![0u8; size];
            std::io::Read::read_exact(&mut reader, &mut chunk).map_err(|e| e.to_string())?;
            raw.extend_from_slice(&chunk);
            let mut crlf = [0u8; 2];
            let _ = std::io::Read::read_exact(&mut reader, &mut crlf);
        }
    } else if let Some(len) = content_length {
        raw = vec![0u8; len];
        std::io::Read::read_exact(&mut reader, &mut raw).map_err(|e| e.to_string())?;
    } else {
        let _ = std::io::Read::read_to_end(&mut reader, &mut raw);
    }
    Ok((
        status,
        String::from_utf8_lossy(&raw).to_string(),
        status_line,
    ))
}

fn http_get(url: &str) -> Result<(u16, String, String), String> {
    http_request("GET", url, &[], None)
}

fn get_json(url: &str) -> Value {
    let (status, body, _) = http_get(url).expect("GET request");
    assert_eq!(status, 200, "GET {url} -> {status}: {body}");
    serde_json::from_str(&body).expect("json body")
}

fn data_of(value: &Value) -> &Value {
    assert_eq!(value["ok"], true, "unexpected error: {value}");
    &value["data"]
}

fn api_post(url: &str, payload: Value) -> (u16, Value) {
    let (status, body, _) = http_request(
        "POST",
        url,
        &[("x-divertlink-console", "1")],
        Some(&payload.to_string()),
    )
    .expect("POST request");
    let parsed = serde_json::from_str(&body).unwrap_or(json!({"raw": body}));
    (status, parsed)
}

fn api_put(url: &str, payload: Value) -> (u16, Value) {
    let (status, body, _) = http_request(
        "PUT",
        url,
        &[("x-divertlink-console", "1")],
        Some(&payload.to_string()),
    )
    .expect("PUT request");
    let parsed = serde_json::from_str(&body).unwrap_or(json!({"raw": body}));
    (status, parsed)
}

fn api_delete(url: &str) -> (u16, Value) {
    let (status, body, _) = http_request("DELETE", url, &[("x-divertlink-console", "1")], None)
        .expect("DELETE request");
    let parsed = serde_json::from_str(&body).unwrap_or(json!({"raw": body}));
    (status, parsed)
}

#[test]
fn console_serves_ui_api_and_security_guards() {
    let tmp = tempfile::TempDir::new().unwrap();
    let store = tmp.path().join("profiles.json");
    let store_arg = store.to_str().unwrap().to_string();
    let console = ConsoleProc::start(&["--store", &store_arg]);

    // UI shell + PWA assets.
    let (status, body, _) = http_get(&console.base).unwrap();
    assert_eq!(status, 200);
    assert!(body.contains("DivertLink"), "index.html missing title");
    assert!(body.contains("app.js"));
    for asset in [
        "app.js",
        "style.css",
        "manifest.webmanifest",
        "sw.js",
        "icon.svg",
    ] {
        let (status, _, _) = http_get(&format!("{}{}", console.base, asset)).unwrap();
        assert_eq!(status, 200, "asset {asset}");
    }
    let (status, _, _) = http_get(&format!("{}nope", console.base)).unwrap();
    assert_eq!(status, 404);

    // API state.
    let state = get_json(&format!("{}api/state", console.base));
    assert_eq!(state["data"]["profiles"].as_array().unwrap().len(), 0);

    // CSRF guard: mutations require the console header.
    let (status, _, _) = http_request(
        "POST",
        &format!("{}api/token", console.base),
        &[],
        Some("{}"),
    )
    .unwrap();
    assert_eq!(
        status, 403,
        "mutation without the console header must be rejected"
    );

    // Token generation works with the header.
    let (status, body) = api_post(
        &format!("{}api/token", console.base),
        json!({"prefix": "agt_"}),
    );
    assert_eq!(status, 200);
    assert!(data_of(&body)["token"]
        .as_str()
        .unwrap()
        .starts_with("agt_"));

    // Project wizard generates configs and registers profiles.
    let dir = tmp.path().join("project");
    let (status, body) = api_post(
        &format!("{}api/init", console.base),
        json!({
            "dir": dir.to_str().unwrap(),
            "domain": "localhost",
            "role": "both",
            "mtls": false,
            "force": true
        }),
    );
    assert_eq!(status, 200, "{body}");
    let init = data_of(&body);
    assert!(Path::new(init["server_toml"].as_str().unwrap()).exists());
    assert!(Path::new(init["agent_toml"].as_str().unwrap()).exists());
    assert!(Path::new(init["ca_cert"].as_str().unwrap()).exists());
    assert!(init["api_key"].as_str().unwrap().starts_with("dlk_"));

    let state = get_json(&format!("{}api/state", console.base));
    let profiles = state["data"]["profiles"].as_array().unwrap();
    assert_eq!(profiles.len(), 2, "init should register two profiles");
    let server_id = profiles.iter().find(|p| p["role"] == "server").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let agent_id = profiles.iter().find(|p| p["role"] == "agent").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        profiles[0]["summary"]["role"],
        json!(if profiles[0]["role"] == "server" {
            "server"
        } else {
            "agent"
        })
    );

    // Config read + edit.
    let config = get_json(&format!("{}api/profiles/{agent_id}/config", console.base));
    let content = data_of(&config)["content"].as_str().unwrap();
    assert!(content.contains("agent"), "{content}");
    let edited = content.replace("max_inflight = 128", "max_inflight = 256");
    let (status, body) = api_put(
        &format!("{}api/profiles/{agent_id}/config", console.base),
        json!({"content": edited}),
    );
    assert_eq!(status, 200, "{body}");
    let (status, body) = api_put(
        &format!("{}api/profiles/{agent_id}/config", console.base),
        json!({"content": "this is not toml"}),
    );
    assert_eq!(status, 400, "invalid TOML must be rejected: {body}");

    // Adding an unrelated existing config is rejected if the file is missing.
    let (status, _) = api_post(
        &format!("{}api/profiles", console.base),
        json!({"name": "missing", "config": tmp.path().join("nope.toml").to_str().unwrap()}),
    );
    assert_eq!(status, 400);

    // Probe of a closed port returns a structured failure (HTTP 200 + ok=false).
    let (status, body) = api_post(
        &format!("{}api/probe", console.base),
        json!({"endpoint": format!("127.0.0.1:{}", free_port()), "timeout_ms": 500}),
    );
    assert_eq!(status, 200);
    assert_eq!(data_of(&body)["ok"], false);
    assert!(data_of(&body)["error"].is_string());

    // Delete removes the profile but keeps the file on disk.
    let (status, _) = api_delete(&format!("{}api/profiles/{agent_id}", console.base));
    assert_eq!(status, 200);
    let state = get_json(&format!("{}api/state", console.base));
    assert_eq!(state["data"]["profiles"].as_array().unwrap().len(), 1);
    assert!(Path::new(init["agent_toml"].as_str().unwrap()).exists());

    // Stop also works through the API.
    let (status, body) = api_post(
        &format!("{}api/profiles/{server_id}/stop", console.base),
        json!({}),
    );
    assert_eq!(status, 200);
    assert_eq!(data_of(&body)["state"], "stopped");

    console.stop();
    assert!(store.exists());
}

#[test]
fn console_supervises_a_gateway_process() {
    let tmp = tempfile::TempDir::new().unwrap();
    let store_arg = tmp.path().join("profiles.json");
    let console = ConsoleProc::start(&["--store", store_arg.to_str().unwrap()]);

    let dir = tmp.path().join("project");
    let (status, body) = api_post(
        &format!("{}api/init", console.base),
        json!({
            "dir": dir.to_str().unwrap(),
            "domain": "localhost",
            "role": "server",
            "force": true
        }),
    );
    assert_eq!(status, 200, "{body}");
    let server_toml = PathBuf::from(data_of(&body)["server_toml"].as_str().unwrap());
    let server_id = data_of(&body)["profiles"].as_array().unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Rewrite listeners to loopback high ports so the test can start the gateway.
    let admin_port = free_port();
    let http_port = free_port();
    let original = std::fs::read_to_string(&server_toml).unwrap();
    let patched = original
        .replace(
            "tunnel_listen = \"0.0.0.0:443\"",
            "tunnel_listen = \"127.0.0.1:0\"",
        )
        .replace(
            "ingress_listen = \"0.0.0.0:443\"",
            "ingress_listen = \"127.0.0.1:0\"",
        )
        .replace(
            "http_listen = \"127.0.0.1:8080\"",
            &format!("http_listen = \"127.0.0.1:{http_port}\""),
        )
        .replace(
            "admin_listen = \"127.0.0.1:9090\"",
            &format!("admin_listen = \"127.0.0.1:{admin_port}\""),
        );
    let (status, body) = api_put(
        &format!("{}api/profiles/{server_id}/config", console.base),
        json!({"content": patched}),
    );
    assert_eq!(status, 200, "{body}");

    // Start it.
    let (status, body) = api_post(
        &format!("{}api/profiles/{server_id}/start", console.base),
        json!({}),
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(data_of(&body)["state"], "running");
    let pid = data_of(&body)["pid"].as_u64().unwrap();
    assert!(pid > 0);

    // Wait until the gateway admin endpoint is reachable, then verify the
    // console surfaces the live JSON status.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut live = Value::Null;
    while Instant::now() < deadline {
        let status = get_json(&format!("{}api/profiles/{server_id}/status", console.base));
        let data = data_of(&status).clone();
        if data["admin"].is_object() {
            live = data;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        live["admin"].is_object(),
        "console did not report the gateway /status JSON: {live}"
    );
    assert_eq!(live["admin"]["server"]["name"], "divertlink-gateway");
    // No agent is connected in this test, so /readyz is expected to be 503.
    assert_eq!(live["ready"], false);

    // Logs must be captured from the child process.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut log_count = 0usize;
    while Instant::now() < deadline {
        let logs = get_json(&format!(
            "{}api/profiles/{server_id}/logs/history?limit=200",
            console.base
        ));
        log_count = data_of(&logs).as_array().unwrap().len();
        if log_count > 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(log_count > 0, "console captured no child logs");

    // Stop the gateway and confirm the process state settles.
    let (status, _) = api_post(
        &format!("{}api/profiles/{server_id}/stop", console.base),
        json!({}),
    );
    assert_eq!(status, 200);
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut stopped = false;
    while Instant::now() < deadline {
        let state = get_json(&format!("{}api/state", console.base));
        let profiles = state["data"]["profiles"].as_array().unwrap();
        if profiles[0]["status"]["state"] == "stopped" {
            stopped = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(stopped, "gateway process did not stop");
    // The gateway's admin port must be free again.
    let rebind = TcpListener::bind(format!("127.0.0.1:{admin_port}"));
    assert!(rebind.is_ok(), "gateway did not release its admin port");

    console.stop();
}

#[test]
fn console_token_protects_the_api() {
    let tmp = tempfile::TempDir::new().unwrap();
    let store_arg = tmp.path().join("profiles.json");
    let console = ConsoleProc::start_with_token(
        &["--store", store_arg.to_str().unwrap()],
        Some("secret-console-token"),
    );

    // Without a token: 401.
    let (status, _, _) = http_get(&format!("{}api/state", console.base)).unwrap();
    assert_eq!(status, 401);
    // Wrong token: 401.
    let (status, _, _) = http_request(
        "GET",
        &format!("{}api/state", console.base),
        &[("x-console-token", "nope")],
        None,
    )
    .unwrap();
    assert_eq!(status, 401);
    // Correct header token: 200.
    let (status, body, _) = http_request(
        "GET",
        &format!("{}api/state", console.base),
        &[("x-console-token", "secret-console-token")],
        None,
    )
    .unwrap();
    assert_eq!(status, 200, "{body}");
    // Query token works too (used by EventSource).
    let (status, _, _) = http_get(&format!(
        "{}api/state?token=secret-console-token",
        console.base
    ))
    .unwrap();
    assert_eq!(status, 200);
    // The UI shell itself stays reachable (it prompts for the token).
    let (status, _, _) = http_get(&console.base).unwrap();
    assert_eq!(status, 200);

    console.stop();
}
