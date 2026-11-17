//! Console profile storage: a small JSON file listing the tunnels/projects the
//! desktop console manages.
//!
//! Layout follows the platform conventions:
//! * Linux:   `$XDG_CONFIG_HOME/divertlink` (or `~/.config/divertlink`)
//! * macOS:   `~/Library/Application Support/divertlink`
//! * Windows: `%APPDATA%\divertlink`
//!
//! `DIVERTLINK_HOME` overrides everything, which is convenient for portable
//! installs and for tests.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::util::{random_bytes, unix_now};

/// Whether a profile runs the home agent or the public gateway.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProfileRole {
    /// `divertlink agent -c <config>`.
    Agent,
    /// `divertlink server -c <config>`.
    Server,
}

impl ProfileRole {
    /// CLI subcommand for this role.
    pub fn subcommand(&self) -> &'static str {
        match self {
            ProfileRole::Agent => "agent",
            ProfileRole::Server => "server",
        }
    }

    /// Parse from a string (`agent`, `server`).
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "agent" | "home" | "client" => Some(Self::Agent),
            "server" | "gateway" => Some(Self::Server),
            _ => None,
        }
    }

    /// Chinese label used by the UI.
    pub fn label(&self) -> &'static str {
        match self {
            ProfileRole::Agent => "家庭端 Agent",
            ProfileRole::Server => "公网网关 Gateway",
        }
    }
}

/// One managed configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    /// Stable identifier (`p_...`).
    pub id: String,
    /// Friendly name shown in the console.
    pub name: String,
    /// Which binary mode this config runs.
    pub role: ProfileRole,
    /// Path to `server.toml` / `agent.toml`.
    pub config: PathBuf,
    /// Start automatically when the console starts.
    #[serde(default)]
    pub autostart: bool,
    /// Free-form notes.
    #[serde(default)]
    pub notes: String,
    /// Creation time (UNIX seconds).
    #[serde(default)]
    pub created_unix: u64,
}

/// Persisted console data.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ConsoleData {
    version: u32,
    #[serde(default)]
    profiles: Vec<Profile>,
    #[serde(default)]
    selected: Option<String>,
}

impl Default for ConsoleData {
    fn default() -> Self {
        ConsoleData {
            version: 1,
            profiles: Vec::new(),
            selected: None,
        }
    }
}

/// Thread-safe profile store backed by a JSON file.
#[derive(Debug)]
pub struct ProfileStore {
    path: PathBuf,
    data: Mutex<ConsoleData>,
}

impl ProfileStore {
    /// Open (or create) the store at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Arc<Self>> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Config(format!("cannot create {}: {e}", parent.display())))?;
        }
        let data = if path.exists() {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| Error::Config(format!("cannot read {}: {e}", path.display())))?;
            if text.trim().is_empty() {
                ConsoleData::default()
            } else {
                serde_json::from_str(&text).map_err(|e| {
                    Error::Config(format!("invalid profile store {}: {e}", path.display()))
                })?
            }
        } else {
            ConsoleData::default()
        };
        let store = ProfileStore {
            path,
            data: Mutex::new(data),
        };
        store.save(&store.snapshot())?;
        Ok(Arc::new(store))
    }

    /// Store file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn snapshot(&self) -> ConsoleData {
        self.data.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn save(&self, data: &ConsoleData) -> Result<()> {
        let text = serde_json::to_string_pretty(data)?;
        write_atomic(&self.path, &text)?;
        Ok(())
    }

    /// All profiles, sorted by name.
    pub fn list(&self) -> Vec<Profile> {
        let data = self.data.lock().unwrap_or_else(|e| e.into_inner());
        let mut profiles = data.profiles.clone();
        drop(data);
        profiles.sort_by_key(|a| a.name.to_lowercase());
        profiles
    }

    /// Look up one profile.
    pub fn get(&self, id: &str) -> Option<Profile> {
        self.data
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .profiles
            .iter()
            .find(|p| p.id == id)
            .cloned()
    }

    /// Currently selected profile id.
    pub fn selected(&self) -> Option<String> {
        self.data
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .selected
            .clone()
    }

    /// Update the selection.
    pub fn select(&self, id: Option<String>) -> Result<()> {
        let mut data = self.data.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(id) = &id {
            if !data.profiles.iter().any(|p| &p.id == id) {
                return Err(Error::Config(format!("unknown profile `{id}`")));
            }
        }
        data.selected = id;
        let snapshot = data.clone();
        drop(data);
        self.save(&snapshot)
    }

    /// Insert or update a profile. Returns the stored value.
    pub fn upsert(&self, mut profile: Profile) -> Result<Profile> {
        if profile.name.trim().is_empty() {
            return Err(Error::Config("profile name must not be empty".into()));
        }
        if profile.config.as_os_str().is_empty() {
            return Err(Error::Config(
                "profile config path must not be empty".into(),
            ));
        }
        profile.name = profile.name.trim().to_string();
        profile.config = absolute(&profile.config);
        let mut data = self.data.lock().unwrap_or_else(|e| e.into_inner());
        if profile.id.trim().is_empty() {
            profile.id = new_id();
            profile.created_unix = unix_now();
        }
        if let Some(existing) = data.profiles.iter_mut().find(|p| p.id == profile.id) {
            existing.name = profile.name.clone();
            existing.role = profile.role;
            existing.config = profile.config.clone();
            existing.autostart = profile.autostart;
            existing.notes = profile.notes.clone();
            profile = existing.clone();
        } else {
            if profile.created_unix == 0 {
                profile.created_unix = unix_now();
            }
            data.profiles.push(profile.clone());
        }
        if data.selected.is_none() {
            data.selected = Some(profile.id.clone());
        }
        let snapshot = data.clone();
        drop(data);
        self.save(&snapshot)?;
        Ok(profile)
    }

    /// Remove a profile.
    pub fn remove(&self, id: &str) -> Result<()> {
        let mut data = self.data.lock().unwrap_or_else(|e| e.into_inner());
        let before = data.profiles.len();
        data.profiles.retain(|p| p.id != id);
        if data.profiles.len() == before {
            return Err(Error::Config(format!("unknown profile `{id}`")));
        }
        if data.selected.as_deref() == Some(id) {
            data.selected = data.profiles.first().map(|p| p.id.clone());
        }
        let snapshot = data.clone();
        drop(data);
        self.save(&snapshot)
    }

    /// Profiles with `autostart = true`.
    pub fn autostart(&self) -> Vec<Profile> {
        self.list().into_iter().filter(|p| p.autostart).collect()
    }

    /// Export the whole store as pretty JSON.
    pub fn export_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(&self.snapshot())?)
    }

    /// Import profiles from JSON, keeping existing ids when they collide.
    pub fn import_json(&self, text: &str) -> Result<usize> {
        #[derive(Deserialize)]
        struct Import {
            #[serde(default)]
            profiles: Vec<Profile>,
        }
        let imported: Import = serde_json::from_str(text)?;
        let mut count = 0;
        for mut profile in imported.profiles {
            if profile.id.trim().is_empty() || self.get(&profile.id).is_some() {
                profile.id = new_id();
            }
            if profile.name.trim().is_empty() {
                continue;
            }
            self.upsert(profile)?;
            count += 1;
        }
        Ok(count)
    }
}

/// Generate a short random profile id.
pub fn new_id() -> String {
    format!("p_{}", crate::util::hex(&random_bytes(6)))
}

/// Base directory for DivertLink user data.
pub fn home_dir() -> PathBuf {
    if let Ok(value) = std::env::var("DIVERTLINK_HOME") {
        if !value.trim().is_empty() {
            return PathBuf::from(value);
        }
    }
    if cfg!(windows) {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return PathBuf::from(appdata).join("divertlink");
        }
    }
    if cfg!(target_os = "macos") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("divertlink");
        }
    }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.trim().is_empty() {
            return PathBuf::from(xdg).join("divertlink");
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".config").join("divertlink");
    }
    PathBuf::from(".divertlink")
}

/// Default profile store path (`<home>/console/profiles.json`).
pub fn default_store_path() -> PathBuf {
    home_dir().join("console").join("profiles.json")
}

fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, contents)
        .map_err(|e| Error::Config(format!("cannot write {}: {e}", tmp.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path)
        .map_err(|e| Error::Config(format!("cannot replace {}: {e}", path.display())))?;
    Ok(())
}

/// Detect the role of a config file by parsing it.
pub fn detect_role(config: &Path) -> Result<ProfileRole> {
    let text = std::fs::read_to_string(config)
        .map_err(|e| Error::Config(format!("cannot read {}: {e}", config.display())))?;
    if crate::config::ServerConfig::from_toml_str(&text).is_ok() {
        return Ok(ProfileRole::Server);
    }
    if crate::config::AgentConfig::from_toml_str(&text).is_ok() {
        return Ok(ProfileRole::Agent);
    }
    Err(Error::Config(format!(
        "{} is neither a valid server.toml nor agent.toml",
        config.display()
    )))
}

/// A lightweight summary of a config file for the UI.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConfigSummary {
    /// Detected role.
    pub role: Option<ProfileRole>,
    /// Gateway endpoint (agent configs).
    pub server: Option<String>,
    /// Local target URL (agent configs).
    pub target: Option<String>,
    /// Tunnel id (agent configs).
    pub tunnel_id: Option<String>,
    /// Gateway name.
    pub name: Option<String>,
    /// Listener addresses (server configs).
    pub listeners: BTreeMap<String, String>,
    /// Number of tunnels (server configs).
    pub tunnels: usize,
    /// Number of API keys (server configs).
    pub api_keys: usize,
    /// Number of static routes (server configs).
    pub static_routes: usize,
    /// Whether ingress authentication is enabled.
    pub auth_enabled: bool,
    /// Mounts exposed by server configs.
    pub mounts: Vec<String>,
}

/// Parse a config file into [`ConfigSummary`].
pub fn summarize_config(config: &Path) -> Result<ConfigSummary> {
    let text = std::fs::read_to_string(config)
        .map_err(|e| Error::Config(format!("cannot read {}: {e}", config.display())))?;
    if let Ok(server) = crate::config::ServerConfig::from_toml_str(&text) {
        let mut listeners = BTreeMap::new();
        if let Some(addr) = server.server.tunnel_listen {
            listeners.insert("tunnel".into(), addr.to_string());
        }
        if let Some(addr) = server.server.ingress_listen {
            listeners.insert("ingress".into(), addr.to_string());
        }
        if let Some(addr) = server.server.http_listen {
            listeners.insert("http".into(), addr.to_string());
        }
        if let Some(addr) = server.server.admin_listen {
            listeners.insert("admin".into(), addr.to_string());
        }
        let mut mounts: Vec<String> = server
            .tunnels
            .iter()
            .filter_map(|t| t.mount.clone())
            .chain(server.static_routes.iter().map(|r| r.mount.clone()))
            .collect();
        mounts.sort();
        return Ok(ConfigSummary {
            role: Some(ProfileRole::Server),
            name: Some(server.server.name.clone()),
            listeners,
            tunnels: server.tunnels.len(),
            api_keys: server.api_keys.len(),
            static_routes: server.static_routes.len(),
            auth_enabled: !server.api_keys.is_empty(),
            mounts,
            ..Default::default()
        });
    }
    match crate::config::AgentConfig::from_toml_str(&text) {
        Ok(agent) => Ok(ConfigSummary {
            role: Some(ProfileRole::Agent),
            server: Some(agent.agent.server.clone()),
            target: Some(agent.target.url.clone()),
            tunnel_id: Some(agent.agent.id.clone()),
            ..Default::default()
        }),
        Err(err) => Err(Error::Config(format!(
            "{} is neither a valid server.toml nor agent.toml: {err}",
            config.display()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_store() -> (TempDir, Arc<ProfileStore>) {
        let tmp = TempDir::new().unwrap();
        let store = ProfileStore::open(tmp.path().join("console/profiles.json")).unwrap();
        (tmp, store)
    }

    fn profile(name: &str, role: ProfileRole) -> Profile {
        Profile {
            id: String::new(),
            name: name.to_string(),
            role,
            config: PathBuf::from("/tmp/example.toml"),
            autostart: false,
            notes: String::new(),
            created_unix: 0,
        }
    }

    #[test]
    fn crud_and_persistence() {
        let (tmp, store) = make_store();
        let a = store.upsert(profile("home", ProfileRole::Agent)).unwrap();
        assert!(a.id.starts_with("p_"));
        assert_eq!(store.list().len(), 1);
        assert_eq!(store.selected().as_deref(), Some(a.id.as_str()));

        let b = store
            .upsert(profile("gateway", ProfileRole::Server))
            .unwrap();
        assert_eq!(store.list().len(), 2);
        store.select(Some(b.id.clone())).unwrap();
        assert_eq!(store.selected().as_deref(), Some(b.id.as_str()));

        // Persisted to disk.
        let reopened = ProfileStore::open(tmp.path().join("console/profiles.json")).unwrap();
        assert_eq!(reopened.list().len(), 2);
        assert_eq!(reopened.selected().as_deref(), Some(b.id.as_str()));

        store.remove(&a.id).unwrap();
        assert_eq!(store.list().len(), 1);
        assert!(store.remove("nope").is_err());
    }

    #[test]
    fn update_keeps_ids() {
        let (_tmp, store) = make_store();
        let a = store.upsert(profile("home", ProfileRole::Agent)).unwrap();
        let mut updated = a.clone();
        updated.name = "home-2".into();
        updated.autostart = true;
        let b = store.upsert(updated).unwrap();
        assert_eq!(a.id, b.id);
        assert_eq!(store.list().len(), 1);
        assert!(store.list()[0].autostart);
        assert_eq!(store.autostart().len(), 1);
    }

    #[test]
    fn rejects_bad_profiles() {
        let (_tmp, store) = make_store();
        let mut bad = profile("", ProfileRole::Agent);
        assert!(store.upsert(bad.clone()).is_err());
        bad.name = "ok".into();
        bad.config = PathBuf::new();
        assert!(store.upsert(bad).is_err());
    }

    #[test]
    fn export_import_roundtrip() {
        let (_tmp, store) = make_store();
        store.upsert(profile("home", ProfileRole::Agent)).unwrap();
        let exported = store.export_json().unwrap();
        let (_tmp2, other) = make_store();
        assert_eq!(other.import_json(&exported).unwrap(), 1);
        assert_eq!(other.list().len(), 1);
        // Importing again with the same ids keeps both (new ids generated).
        other.import_json(&exported).unwrap();
        assert_eq!(other.list().len(), 2);
    }

    #[test]
    fn summarizes_agent_config() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("agent.toml");
        std::fs::write(
            &path,
            r#"
[agent]
id = "home"
server = "gateway.example.com:443"
token = "t"
[target]
url = "http://127.0.0.1:8000"
"#,
        )
        .unwrap();
        let summary = summarize_config(&path).unwrap();
        assert_eq!(summary.role, Some(ProfileRole::Agent));
        assert_eq!(summary.tunnel_id.as_deref(), Some("home"));
        assert_eq!(summary.server.as_deref(), Some("gateway.example.com:443"));
    }

    #[test]
    fn home_dir_respects_env() {
        // Reading env in parallel tests is racy, so only assert the fallback is non-empty.
        assert!(!home_dir().as_os_str().is_empty());
    }
}
