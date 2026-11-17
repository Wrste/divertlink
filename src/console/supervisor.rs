//! Child-process supervision for the console: start/stop/restart the real
//! `divertlink server` / `divertlink agent` binaries and stream their logs.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use super::profile::{ConfigSummary, Profile, ProfileStore};
use crate::error::{Error, Result};
use crate::util::{random_bytes, unix_now};

/// Maximum number of log lines kept in memory per console session.
const LOG_CAPACITY: usize = 2000;

/// How long to wait for a graceful SIGTERM shutdown before killing the child.
const STOP_GRACE: Duration = Duration::from_secs(8);

/// One captured log line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogLine {
    /// Monotonic sequence number (for incremental fetches).
    pub seq: u64,
    /// UNIX seconds.
    pub at_unix: u64,
    /// Profile id or `_console`.
    pub profile: String,
    /// `stdout`, `stderr` or `console`.
    pub stream: String,
    /// Line content (already trimmed of the trailing newline).
    pub line: String,
}

/// In-memory ring buffer plus broadcast fan-out for live log streaming.
#[derive(Debug)]
pub struct LogBuffer {
    inner: Mutex<Vec<LogLine>>,
    seq: AtomicU64,
    tx: broadcast::Sender<LogLine>,
}

impl LogBuffer {
    fn new() -> Self {
        let (tx, _) = broadcast::channel(512);
        LogBuffer {
            inner: Mutex::new(Vec::with_capacity(LOG_CAPACITY)),
            seq: AtomicU64::new(0),
            tx,
        }
    }

    /// Append a line and notify subscribers.
    pub fn push(&self, profile: &str, stream: &str, line: impl Into<String>) {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let entry = LogLine {
            seq,
            at_unix: unix_now(),
            profile: profile.to_string(),
            stream: stream.to_string(),
            line: line.into(),
        };
        {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if inner.len() >= LOG_CAPACITY {
                inner.remove(0);
            }
            inner.push(entry.clone());
        }
        let _ = self.tx.send(entry);
    }

    /// Lines with `seq > after`, optionally limited to one profile.
    pub fn history(&self, after: u64, profile: Option<&str>, limit: usize) -> Vec<LogLine> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .iter()
            .filter(|l| l.seq > after)
            .filter(|l| profile.map(|p| l.profile == p).unwrap_or(true))
            .take(limit.max(1))
            .cloned()
            .collect()
    }

    /// Subscribe to live lines.
    pub fn subscribe(&self) -> broadcast::Receiver<LogLine> {
        self.tx.subscribe()
    }
}

/// Observable state of a supervised process.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProcStatus {
    /// Profile id.
    pub profile: String,
    /// `stopped`, `starting`, `running`, `stopping` or `failed`.
    pub state: String,
    /// Operating-system process id while running.
    pub pid: Option<u32>,
    /// UNIX seconds when the process was started.
    pub started_unix: Option<u64>,
    /// Last exit code seen by the supervisor.
    pub exit_code: Option<i32>,
    /// Last error (spawn failure or non-zero exit summary).
    pub last_error: Option<String>,
    /// How many times the console restarted this profile.
    pub restarts: u32,
}

impl ProcStatus {
    fn stopped(profile: &str) -> Self {
        ProcStatus {
            profile: profile.to_string(),
            state: "stopped".into(),
            ..Default::default()
        }
    }
}

/// A running child plus the shared status cell the waiter task updates.
#[derive(Debug)]
struct ChildHandle {
    pid: u32,
    status: Arc<Mutex<ProcStatus>>,
}

/// Starts and stops DivertLink child processes.
#[derive(Debug)]
pub struct Supervisor {
    store: Arc<ProfileStore>,
    children: Mutex<HashMap<String, ChildHandle>>,
    logs: Arc<LogBuffer>,
    shutdown: CancellationToken,
    started: Instant,
}

impl Supervisor {
    /// Create a supervisor for `store`, shutting all children down when
    /// `shutdown` is cancelled.
    pub fn new(store: Arc<ProfileStore>, shutdown: CancellationToken) -> Arc<Self> {
        let supervisor = Arc::new(Supervisor {
            store,
            children: Mutex::new(HashMap::new()),
            logs: Arc::new(LogBuffer::new()),
            shutdown,
            started: Instant::now(),
        });
        let weak = Arc::downgrade(&supervisor);
        let token = supervisor.shutdown.clone();
        tokio::spawn(async move {
            token.cancelled().await;
            if let Some(supervisor) = weak.upgrade() {
                supervisor.stop_all().await;
            }
        });
        supervisor
    }

    /// Shared log buffer.
    pub fn logs(&self) -> Arc<LogBuffer> {
        self.logs.clone()
    }

    /// Current status of one profile (from the supervisor cache).
    pub fn status(&self, profile: &str) -> ProcStatus {
        let children = self.children.lock().unwrap_or_else(|e| e.into_inner());
        match children.get(profile) {
            Some(handle) => handle
                .status
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            None => ProcStatus::stopped(profile),
        }
    }

    /// Status for all profiles managed by the store.
    pub fn all_status(&self) -> Vec<ProcStatus> {
        self.store
            .list()
            .iter()
            .map(|p| self.status(&p.id))
            .collect()
    }

    /// Start a profile.
    pub async fn start(&self, profile: &Profile) -> Result<ProcStatus> {
        {
            let children = self.children.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(handle) = children.get(&profile.id) {
                let status = handle.status.lock().unwrap_or_else(|e| e.into_inner());
                if status.state == "running" {
                    return Ok(status.clone());
                }
            }
        }
        if !profile.config.exists() {
            return Err(Error::Config(format!(
                "config file {} does not exist",
                profile.config.display()
            )));
        }
        let summary = super::profile::summarize_config(&profile.config)?;
        if let Some(detected) = summary.role {
            if detected != profile.role {
                return Err(Error::Config(format!(
                    "profile `{}` is configured as {:?} but {} is a {:?} config",
                    profile.name,
                    profile.role,
                    profile.config.display(),
                    detected
                )));
            }
        }
        let exe = std::env::current_exe()
            .map_err(|e| Error::Internal(format!("cannot locate divertlink binary: {e}")))?;
        let mut command = Command::new(exe);
        command
            .arg(profile.role.subcommand())
            .arg("-c")
            .arg(&profile.config)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("DIVERTLINK_CONSOLE", "1");
        if let Some(dir) = profile.config.parent() {
            command.current_dir(dir);
        }
        let mut child = command
            .spawn()
            .map_err(|e| Error::Internal(format!("cannot start {}: {e}", profile.name)))?;
        let pid = child
            .id()
            .ok_or_else(|| Error::Internal("child process has no pid".into()))?;
        let status = Arc::new(Mutex::new(ProcStatus {
            profile: profile.id.clone(),
            state: "running".into(),
            pid: Some(pid),
            started_unix: Some(unix_now()),
            exit_code: None,
            last_error: None,
            restarts: 0,
        }));
        self.logs.push(
            &profile.id,
            "console",
            format!(
                "started {} ({:?}) pid={} config={}",
                profile.name,
                profile.role,
                pid,
                profile.config.display()
            ),
        );

        if let Some(stdout) = child.stdout.take() {
            let logs = self.logs.clone();
            let id = profile.id.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    logs.push(&id, "stdout", line);
                }
            });
        }
        if let Some(stderr) = child.stderr.take() {
            let logs = self.logs.clone();
            let id = profile.id.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    logs.push(&id, "stderr", line);
                }
            });
        }

        let status_cell = status.clone();
        let logs = self.logs.clone();
        let id = profile.id.clone();
        let name = profile.name.clone();
        tokio::spawn(async move {
            let exit = child.wait().await;
            let (code, error) = match &exit {
                Ok(status) => {
                    let code = status.code();
                    let error = if status.success() {
                        None
                    } else {
                        Some(format!(
                            "process exited with {}",
                            code.map(|c| c.to_string())
                                .unwrap_or_else(|| "signal".into())
                        ))
                    };
                    (code, error)
                }
                Err(err) => (None, Some(format!("wait failed: {err}"))),
            };
            {
                let mut status = status_cell.lock().unwrap_or_else(|e| e.into_inner());
                status.state = if error.is_some() {
                    "failed".into()
                } else {
                    "stopped".into()
                };
                status.pid = None;
                status.exit_code = code;
                if error.is_some() {
                    status.last_error = error.clone();
                }
            }
            logs.push(
                &id,
                "console",
                format!(
                    "{} exited: code={:?}{}",
                    name,
                    code,
                    error.map(|e| format!(" ({e})")).unwrap_or_default()
                ),
            );
        });

        let status_snapshot = status.lock().unwrap_or_else(|e| e.into_inner()).clone();
        self.children
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(profile.id.clone(), ChildHandle { pid, status });
        let _ = &summary;
        Ok(status_snapshot)
    }

    /// Stop a profile gracefully (SIGTERM, then SIGKILL after the internal grace period).
    pub async fn stop(&self, profile_id: &str) -> Result<ProcStatus> {
        let pid = {
            let mut children = self.children.lock().unwrap_or_else(|e| e.into_inner());
            match children.remove(profile_id) {
                Some(handle) => {
                    {
                        let mut status = handle.status.lock().unwrap_or_else(|e| e.into_inner());
                        status.state = "stopping".into();
                    }
                    handle.pid
                }
                None => return Ok(ProcStatus::stopped(profile_id)),
            }
        };
        self.logs
            .push(profile_id, "console", format!("stopping pid={pid}"));
        terminate(pid, STOP_GRACE).await;
        self.logs
            .push(profile_id, "console", format!("stopped pid={pid}"));
        Ok(ProcStatus {
            profile: profile_id.to_string(),
            state: "stopped".into(),
            ..Default::default()
        })
    }

    /// Restart a profile.
    pub async fn restart(&self, profile: &Profile) -> Result<ProcStatus> {
        let _ = self.stop(&profile.id).await;
        tokio::time::sleep(Duration::from_millis(250)).await;
        self.start(profile).await
    }

    /// Start every `autostart` profile.
    pub async fn start_autostart(&self) {
        let mut started = 0usize;
        for profile in self.store.autostart() {
            match self.start(&profile).await {
                Ok(_) => started += 1,
                Err(err) => {
                    self.logs
                        .push(&profile.id, "console", format!("autostart failed: {err}"))
                }
            }
        }
        if started > 0 {
            self.logs.push(
                "_console",
                "console",
                format!("autostarted {started} profile(s)"),
            );
        }
    }

    /// Stop all children (used on console shutdown).
    pub async fn stop_all(&self) {
        let ids: Vec<String> = {
            let children = self.children.lock().unwrap_or_else(|e| e.into_inner());
            children.keys().cloned().collect()
        };
        for id in ids {
            let _ = self.stop(&id).await;
        }
    }

    /// Summary of a profile's config for the UI.
    pub fn summary(&self, profile: &Profile) -> Option<ConfigSummary> {
        super::profile::summarize_config(&profile.config).ok()
    }

    /// Nonce used by the UI to correlate console sessions.
    pub fn nonce(&self) -> String {
        format!("c_{}", crate::util::hex(&random_bytes(4)))
    }

    /// When the supervisor was created.
    pub fn started_at(&self) -> Instant {
        self.started
    }
}

/// Send SIGTERM (Unix) or terminate (Windows), waiting `grace` before a hard kill.
async fn terminate(pid: u32, grace: Duration) {
    #[cfg(unix)]
    {
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
        tokio::time::sleep(grace).await;
        // If still alive, escalate. `kill(pid, 0)` checks existence.
        let alive = unsafe { libc::kill(pid as i32, 0) } == 0;
        if alive {
            unsafe {
                libc::kill(pid as i32, libc::SIGKILL);
            }
        }
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .status();
        let _ = grace;
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (pid, grace);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::console::profile::{Profile, ProfileRole, ProfileStore};
    use tempfile::TempDir;

    #[test]
    fn log_buffer_history_and_filtering() {
        let buffer = LogBuffer::new();
        buffer.push("a", "stdout", "one");
        buffer.push("a", "stderr", "two");
        buffer.push("b", "stdout", "three");
        let all = buffer.history(0, None, 100);
        assert_eq!(all.len(), 3);
        let only_a = buffer.history(0, Some("a"), 100);
        assert_eq!(only_a.len(), 2);
        let after = buffer.history(all[1].seq, None, 100);
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].line, "three");
        assert!(buffer.history(0, None, 2).len() == 2);
    }

    #[test]
    fn log_buffer_is_bounded() {
        let buffer = LogBuffer::new();
        for i in 0..(LOG_CAPACITY + 50) {
            buffer.push("p", "stdout", format!("line {i}"));
        }
        let all = buffer.history(0, None, LOG_CAPACITY * 2);
        assert_eq!(all.len(), LOG_CAPACITY);
        assert!(all
            .last()
            .unwrap()
            .line
            .ends_with(&format!("{}", LOG_CAPACITY + 49)));
    }

    #[tokio::test]
    async fn status_defaults_to_stopped() {
        let tmp = TempDir::new().unwrap();
        let store = ProfileStore::open(tmp.path().join("profiles.json")).unwrap();
        let supervisor = Supervisor::new(store, CancellationToken::new());
        let status = supervisor.status("unknown");
        assert_eq!(status.state, "stopped");
        assert!(supervisor.all_status().is_empty());
    }

    #[tokio::test]
    async fn starting_missing_config_fails_cleanly() {
        let tmp = TempDir::new().unwrap();
        let store = ProfileStore::open(tmp.path().join("profiles.json")).unwrap();
        let supervisor = Supervisor::new(store, CancellationToken::new());
        let profile = Profile {
            id: "p_1".into(),
            name: "missing".into(),
            role: ProfileRole::Agent,
            config: tmp.path().join("nope.toml"),
            autostart: false,
            notes: String::new(),
            created_unix: 0,
        };
        let err = supervisor.start(&profile).await.unwrap_err();
        assert!(err.to_string().contains("does not exist"), "{err}");
    }
}
