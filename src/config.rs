//! Persisted configuration, saved serve profiles, and the XDG paths ft-man uses.
//!
//! Everything lives under the standard XDG roots so the tool leaves no surprises on a
//! server: config in `~/.config/ft-man`, mutable state and logs in `~/.local/state/ft-man`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::knobs::ServeConfig;

/// Config root. `FT_MAN_CONFIG_DIR` overrides it, which is how one machine runs several
/// independent ft-man setups (say, one per GPU) without them sharing profiles.
pub fn config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("FT_MAN_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("ft-man")
}

/// State root, holding the serve state file and every captured log.
/// `FT_MAN_STATE_DIR` overrides it, for the same reason.
pub fn state_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("FT_MAN_STATE_DIR") {
        return PathBuf::from(dir);
    }
    dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ft-man")
}

pub fn log_dir() -> PathBuf {
    state_dir().join("logs")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

pub fn profiles_path() -> PathBuf {
    config_dir().join("profiles.toml")
}

pub fn serve_state_path() -> PathBuf {
    state_dir().join("serve.json")
}

// ---------------------------------------------------------------- config

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub freetoken: FreetokenCfg,
    pub server: ServerCfg,
    pub library: LibraryCfg,
    pub hub: HubCfg,
    pub ui: UiCfg,
}

/// How to invoke FreeToken. `ft` is normally on PATH inside the venv it was installed
/// into; pointing `venv` at that venv is enough and is the friendliest option, since it
/// also makes `python -m freetoken.cli` available as a fallback.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FreetokenCfg {
    /// Explicit path to the `ft` executable. Overrides `venv` and PATH lookup.
    pub binary: Option<PathBuf>,
    /// A virtualenv root; `<venv>/bin/ft` and `<venv>/bin/python` are used from it.
    pub venv: Option<PathBuf>,
    /// Extra environment variables applied to every spawned FreeToken process.
    pub env: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerCfg {
    /// Host ft-man polls for engine telemetry. Also the default `ft serve --host`.
    pub host: String,
    /// Port ft-man polls. Also the default `ft serve --port`.
    pub port: u16,
    /// Poll period for /health, /v1/stats and /v1/cache/status, in milliseconds.
    pub poll_ms: u64,
    /// HTTP timeout for those polls, in milliseconds.
    pub timeout_ms: u64,
}

impl Default for ServerCfg {
    fn default() -> Self {
        Self { host: "127.0.0.1".into(), port: 1919, poll_ms: 1000, timeout_ms: 4000 }
    }
}

impl ServerCfg {
    pub fn base_url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LibraryCfg {
    /// Directories scanned for checkpoints. Each is searched one level deep, plus the
    /// two-level `org/model` layout the Hugging Face cache and mirrors use.
    pub roots: Vec<PathBuf>,
    /// Where Hub downloads land, and the default parent for `ft checkpoint --out`.
    pub download_dir: PathBuf,
}

impl Default for LibraryCfg {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let models = home.join("models");
        Self { roots: vec![models.clone()], download_dir: models }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HubCfg {
    /// Hugging Face endpoint; override for a mirror.
    pub endpoint: String,
    /// Access token for gated or private repos. `HF_TOKEN` in the environment wins.
    pub token: Option<String>,
    /// Concurrent file downloads.
    pub concurrency: usize,
    /// Glob-ish suffixes skipped by default (duplicate weight formats, mostly).
    pub ignore: Vec<String>,
}

impl Default for HubCfg {
    fn default() -> Self {
        Self {
            endpoint: "https://huggingface.co".into(),
            token: None,
            concurrency: 4,
            ignore: vec![
                "*.bin".into(),
                "*.pth".into(),
                "*.msgpack".into(),
                "*.h5".into(),
                "*.onnx".into(),
            ],
        }
    }
}

impl HubCfg {
    /// The effective token: `HF_TOKEN`, then `HUGGING_FACE_HUB_TOKEN`, then config, then
    /// the token the `hf` CLI writes.
    pub fn effective_token(&self) -> Option<String> {
        for var in ["HF_TOKEN", "HUGGING_FACE_HUB_TOKEN"] {
            if let Ok(v) = std::env::var(var) {
                if !v.trim().is_empty() {
                    return Some(v.trim().to_string());
                }
            }
        }
        if let Some(t) = self.token.as_ref().filter(|t| !t.trim().is_empty()) {
            return Some(t.trim().to_string());
        }
        let path = dirs::home_dir()?.join(".cache/huggingface/token");
        let raw = std::fs::read_to_string(path).ok()?;
        let t = raw.trim().to_string();
        (!t.is_empty()).then_some(t)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiCfg {
    /// `auto`, `dark`, `light`, or `mono`.
    pub theme: String,
    /// Frame budget for the render loop, in milliseconds.
    pub tick_ms: u64,
    /// Lines of engine output kept in memory for the Logs view.
    pub log_capacity: usize,
    /// Ask before stopping the engine or deleting anything.
    pub confirm_destructive: bool,
}

impl Default for UiCfg {
    fn default() -> Self {
        Self { theme: "auto".into(), tick_ms: 200, log_capacity: 5000, confirm_destructive: true }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = config_path();
        if !path.exists() {
            let cfg = Self::default();
            cfg.save().ok();
            return Ok(cfg);
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self) -> Result<()> {
        let path = config_path();
        write_atomic(&path, &toml::to_string_pretty(self)?)
    }
}

// ---------------------------------------------------------------- profiles

/// A named, reusable `ft serve` configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    #[serde(default)]
    pub notes: String,
    /// Knob key -> value, as stored by [`ServeConfig`].
    #[serde(default)]
    pub serve: ServeConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Profiles {
    #[serde(rename = "profile")]
    pub items: Vec<Profile>,
    /// Name of the profile selected when ft-man last exited.
    pub last_used: Option<String>,
}

impl Profiles {
    pub fn load() -> Result<Self> {
        let path = profiles_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self) -> Result<()> {
        write_atomic(&profiles_path(), &toml::to_string_pretty(self)?)
    }

    pub fn get(&self, name: &str) -> Option<&Profile> {
        self.items.iter().find(|p| p.name == name)
    }

    /// Insert or replace by name, keeping the list sorted for a stable UI order.
    pub fn upsert(&mut self, profile: Profile) {
        match self.items.iter_mut().find(|p| p.name == profile.name) {
            Some(slot) => *slot = profile,
            None => self.items.push(profile),
        }
        self.items.sort_by(|a, b| a.name.cmp(&b.name));
    }

    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.items.len();
        self.items.retain(|p| p.name != name);
        self.items.len() != before
    }
}

/// Point the config and state roots at a scratch directory, once per test process, so
/// tests never touch the developer's real ft-man configuration. Idempotent and shared by
/// every test module, since the roots are process-global.
#[cfg(test)]
pub fn isolate_paths_for_tests() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let dir = std::env::temp_dir().join(format!("ft-man-tests-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("state")).unwrap();
        std::env::set_var("FT_MAN_CONFIG_DIR", &dir);
        std::env::set_var("FT_MAN_STATE_DIR", dir.join("state"));
    });
}

/// Write via a temp file in the same directory, then rename — so an interrupted save
/// never leaves a half-written config behind.
pub fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, contents).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("renaming into {}", path.display()))?;
    Ok(())
}
