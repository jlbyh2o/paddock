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
    pub templates: TemplatesCfg,
    pub convert: ConvertCfg,
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
    /// Bind address ft-man defaults `ft serve --host` to. Also what ft-man polls for
    /// engine telemetry, by way of [`poll_host`] — a wildcard bind is not a destination.
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
        Self { host: "0.0.0.0".into(), port: 1919, poll_ms: 1000, timeout_ms: 4000 }
    }
}

impl ServerCfg {
    pub fn base_url(&self) -> String {
        format!("http://{}:{}", poll_host(&self.host), self.port)
    }
}

/// The address to talk to an engine bound to `host`. A bind address of 0.0.0.0 means
/// "every interface", which is not itself a usable destination, so reach such an engine
/// over the loopback it is also listening on.
pub fn poll_host(host: &str) -> &str {
    match host.trim() {
        "0.0.0.0" | "::" | "*" => "127.0.0.1",
        h => h,
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

/// A Hugging Face token plus where it came from. Resolved once at startup and passed
/// around, so every part of the UI agrees about whether there is a token — the message
/// on the Hub tab and the client that does the downloading must never disagree.
#[derive(Debug, Clone)]
pub struct HubToken {
    pub value: String,
    /// Human-readable provenance, e.g. `the HF_TOKEN environment variable`.
    pub source: &'static str,
}

impl HubCfg {
    /// The effective token: `HF_TOKEN`, then `HUGGING_FACE_HUB_TOKEN`, then `hub.token`
    /// in the config, then the token the `hf` CLI caches.
    pub fn resolve_token(&self) -> Option<HubToken> {
        for (var, source) in [
            ("HF_TOKEN", "the HF_TOKEN environment variable"),
            ("HUGGING_FACE_HUB_TOKEN", "the HUGGING_FACE_HUB_TOKEN environment variable"),
        ] {
            if let Ok(v) = std::env::var(var) {
                let v = v.trim();
                if !v.is_empty() {
                    return Some(HubToken { value: v.to_string(), source });
                }
            }
        }
        if let Some(t) = self.token.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
            return Some(HubToken { value: t.to_string(), source: "hub.token in the config" });
        }
        let raw =
            std::fs::read_to_string(dirs::home_dir()?.join(".cache/huggingface/token")).ok()?;
        let t = raw.trim();
        (!t.is_empty())
            .then(|| HubToken { value: t.to_string(), source: "the token cached by the hf CLI" })
    }
}

/// Checkpoint conversion to FTW.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ConvertCfg {
    /// Ask FreeToken what it makes of a checkpoint before converting it. Costs a few
    /// seconds against a job that otherwise runs for minutes and writes tens of
    /// gigabytes before discovering it cannot read the experts.
    pub preflight: bool,
}

impl Default for ConvertCfg {
    fn default() -> Self {
        Self { preflight: true }
    }
}

/// Chat template overrides. FreeToken has no flag for this, so a template is applied by
/// writing `chat_template.jinja` into the checkpoint directory; see `crate::templates`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TemplatesCfg {
    /// Hugging Face repos offered when fetching templates. Any repo holding `.jinja`
    /// files works; these are just the ones listed first.
    pub sources: Vec<String>,
    /// Run a real `apply_chat_template` render through FreeToken's Python before writing
    /// a template into a checkpoint. Catches the failures a text check cannot.
    pub preflight: bool,
}

impl Default for TemplatesCfg {
    fn default() -> Self {
        Self { sources: vec!["peculiar-ragdoll/Qwen-Sharp-Chat-Templates".into()], preflight: true }
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

/// Serialize tests that touch the serve state file.
///
/// There is exactly one `serve.json` per state root and the root is process-global, so
/// tests that write it — and `App::new`, which reads it to re-adopt an engine — must not
/// run concurrently or they clobber each other's records. The guard is held across
/// awaits while a test drives a child process, so this is tokio's mutex rather than the
/// standard one.
#[cfg(test)]
pub async fn lock_serve_state() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    LOCK.lock().await
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `resolve_token` reads process-global environment, so these cases run under one
    /// lock and one test rather than racing each other.
    #[test]
    fn hub_token_resolution_prefers_env_then_config() {
        // Start from a known state: neither variable set.
        std::env::remove_var("HF_TOKEN");
        std::env::remove_var("HUGGING_FACE_HUB_TOKEN");

        // A token in the config file is honored. This is the case that regressed: the
        // Hub view used to consult a freshly defaulted Config, so a configured token
        // read as "no token found".
        let mut cfg = HubCfg { token: Some("hf_from_config".into()), ..Default::default() };
        let resolved = cfg.resolve_token().expect("a configured token must be found");
        assert_eq!(resolved.value, "hf_from_config");
        assert_eq!(resolved.source, "hub.token in the config");

        // Whitespace around a pasted token is stripped, not treated as part of it.
        cfg.token = Some("  hf_padded\n".into());
        assert_eq!(cfg.resolve_token().unwrap().value, "hf_padded");

        // A blank entry is the same as no entry, and must not shadow the other sources.
        cfg.token = Some("   ".into());
        std::env::set_var("HF_TOKEN", "hf_from_env");
        assert_eq!(cfg.resolve_token().unwrap().value, "hf_from_env");

        // The environment wins over the config file.
        cfg.token = Some("hf_from_config".into());
        let resolved = cfg.resolve_token().unwrap();
        assert_eq!(resolved.value, "hf_from_env");
        assert_eq!(resolved.source, "the HF_TOKEN environment variable");

        // An empty variable is ignored rather than treated as a token.
        std::env::set_var("HF_TOKEN", "");
        assert_eq!(cfg.resolve_token().unwrap().value, "hf_from_config");

        std::env::remove_var("HF_TOKEN");
    }

    #[test]
    fn a_config_round_trips_through_toml() {
        let mut cfg = Config::default();
        cfg.hub.token = Some("hf_secret".into());
        cfg.server.port = 1920;
        let text = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.hub.token.as_deref(), Some("hf_secret"));
        assert_eq!(back.server.port, 1920);
    }

    /// The config rejects unknown keys, so a token written under the wrong section fails
    /// loudly at startup instead of being silently dropped.
    #[test]
    fn a_misplaced_token_key_is_a_hard_error() {
        let err = toml::from_str::<Config>("[hub]\ntoken = \"x\"\nhf_token = \"y\"\n")
            .expect_err("an unknown key must not be ignored");
        assert!(err.to_string().contains("hf_token"), "{err}");
    }
}
