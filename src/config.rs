//! Persisted configuration, saved serve profiles, and the XDG paths paddock uses.
//!
//! Everything lives under the standard XDG roots so the tool leaves no surprises on a
//! server: config in `~/.config/paddock`, mutable state and logs in `~/.local/state/paddock`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::knobs::ServeConfig;

/// What this program was called before it was `paddock`, and so the directory name and
/// environment variables an existing install still has on disk. See [`migrate_legacy_dirs`].
pub const LEGACY_NAME: &str = "ft-man";

/// Config root. `PADDOCK_CONFIG_DIR` overrides it, which is how one machine runs several
/// independent paddock setups (say, one per GPU) without them sharing profiles.
///
/// `FT_MAN_CONFIG_DIR` is still honored when the new name is unset: that variable is the
/// kind of thing that ends up in a systemd unit or a shell profile, and an upgrade that
/// silently ignored it would quietly start a second, empty configuration.
pub fn config_dir() -> PathBuf {
    if let Some(dir) = env_dir("PADDOCK_CONFIG_DIR", "FT_MAN_CONFIG_DIR") {
        return dir;
    }
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("paddock")
}

/// State root, holding the serve state file and every captured log.
/// `PADDOCK_STATE_DIR` overrides it, for the same reason.
pub fn state_dir() -> PathBuf {
    if let Some(dir) = env_dir("PADDOCK_STATE_DIR", "FT_MAN_STATE_DIR") {
        return dir;
    }
    state_root().join("paddock")
}

fn state_root() -> PathBuf {
    dirs::state_dir().or_else(dirs::data_local_dir).unwrap_or_else(|| PathBuf::from("."))
}

fn env_dir(name: &str, legacy: &str) -> Option<PathBuf> {
    std::env::var_os(name).or_else(|| std::env::var_os(legacy)).map(PathBuf::from)
}

/// Move a pre-rename install into place, once.
///
/// Called at startup, before anything reads either directory. The state directory is the
/// one that matters: it holds `serve.json`, which is how a restarted paddock re-adopts an
/// engine that is still running, plus `costs.json` — measurements that can only be taken
/// from a live engine and would otherwise have to be earned again. The config directory
/// holds the saved profiles.
///
/// Renaming rather than copying, so there is exactly one of each afterwards and no
/// question about which is authoritative. Skipped entirely when the new directory already
/// exists: that means either this already ran, or the user has both, and in neither case
/// should the old one win. A failure here is not fatal — paddock starts with fresh
/// defaults, which is what it would have done anyway.
pub fn migrate_legacy_dirs() {
    // Only for the default locations. An explicit PADDOCK_*_DIR is a deliberate choice
    // about where state lives, and moving something into it would be a surprise.
    if std::env::var_os("PADDOCK_CONFIG_DIR").is_none()
        && std::env::var_os("FT_MAN_CONFIG_DIR").is_none()
    {
        if let Some(base) = dirs::config_dir() {
            migrate_one(&base.join(LEGACY_NAME), &base.join("paddock"));
        }
    }
    if std::env::var_os("PADDOCK_STATE_DIR").is_none()
        && std::env::var_os("FT_MAN_STATE_DIR").is_none()
    {
        migrate_one(&state_root().join(LEGACY_NAME), &state_root().join("paddock"));
    }
}

/// Reported on stderr rather than through `tracing`, because this necessarily runs before
/// logging is initialized and a `tracing` call from here goes nowhere — which is how the
/// first real migration completed without saying a word.
///
/// That order is not incidental and must not be swapped to fix the logging: `init_logging`
/// opens a file under the *new* state directory, which creates it, which would make the
/// `to.exists()` check below decline the move and quietly strand the old install.
fn migrate_one(from: &Path, to: &Path) {
    if to.exists() || !from.is_dir() {
        return;
    }
    if let Some(parent) = to.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::rename(from, to) {
        Ok(()) => eprintln!("paddock: moved {} to {}", from.display(), to.display()),
        Err(e) => eprintln!(
            "paddock: could not move {} to {} ({e}); starting with fresh defaults. The old \
             directory is untouched.",
            from.display(),
            to.display(),
        ),
    }
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
    pub web: WebCfg,
}

/// How to invoke FreeToken. `ft` is normally on PATH inside the venv it was installed
/// into; pointing `venv` at that venv is enough and is the friendliest option, since it
/// also makes `python -m freetoken.cli` available as a fallback.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FreetokenCfg {
    /// Explicit path to the `ft` executable. Overrides `venv` and PATH lookup.
    pub binary: Option<PathBuf>,
    /// A virtualenv root; `<venv>/bin/ft` and `<venv>/bin/python` are used from it.
    pub venv: Option<PathBuf>,
    /// Extra environment variables applied to every spawned FreeToken process.
    pub env: Vec<(String, String)>,
    /// The FreeToken git checkout this machine builds from, when it is not the directory
    /// `venv` or `binary` sits inside. Normally unset: a `.venv` made in the clone is
    /// found without help.
    pub checkout: Option<PathBuf>,
    /// Minutes between `git fetch` checks of that checkout. This is the one poll that
    /// leaves the machine, so it is deliberately slow. Zero checks once at startup and
    /// never again.
    pub checkout_poll_min: u64,
}

impl Default for FreetokenCfg {
    fn default() -> Self {
        Self { binary: None, venv: None, env: Vec::new(), checkout: None, checkout_poll_min: 30 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerCfg {
    /// Bind address paddock defaults `ft serve --host` to. Also what paddock polls for
    /// engine telemetry, by way of [`poll_host`] — a wildcard bind is not a destination.
    pub host: String,
    /// Port paddock polls. Also the default `ft serve --port`.
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
    /// two-level `org/model` layout `hf download --local-dir` and mirrors produce. An
    /// entry named `models--org--name` is recognized as a Hugging Face hub cache entry
    /// and resolved through its ref, so a cache directory can be listed here directly.
    pub roots: Vec<PathBuf>,
    /// Where a plain (non-cache) download lands. Downloads from the Hub tab go to the
    /// hub cache instead; this remains the home for checkpoints placed by hand.
    pub download_dir: PathBuf,
    /// The Hugging Face hub cache to read and download into.
    ///
    /// Set this when the cache is not where the environment says it is — which is most of
    /// the time on a server. `HF_HOME` is exported by a shell profile, so it reaches an
    /// interactive login and nothing else: not a session opened before the profile was
    /// written, not a systemd unit, not a terminal an editor spawned. paddock would then
    /// silently read an empty cache in the home directory and report a library of nothing,
    /// and price a download against the wrong filesystem's free space.
    pub hub_cache: Option<PathBuf>,
    /// Where FTW builds are written. Defaults to [`Self::download_dir`].
    ///
    /// Never beside the source checkpoint: a hub-cache checkpoint's sibling is inside
    /// `snapshots/`, and that tree belongs to `huggingface_hub` -- a directory it did not
    /// write is invisible to `hf cache scan` and at risk from `hf cache delete`.
    pub ftw_dir: Option<PathBuf>,
}

impl Default for LibraryCfg {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let models = home.join("models");
        Self { roots: vec![models.clone()], download_dir: models, hub_cache: None, ftw_dir: None }
    }
}

impl LibraryCfg {
    /// Every root actually scanned: the configured ones plus the Hugging Face hub cache,
    /// which is where anything downloaded by `hf`, `from_pretrained`, or another engine
    /// on this machine already is. Added implicitly so the common case needs no config,
    /// and deduplicated so listing it explicitly is harmless.
    pub fn effective_roots(&self) -> Vec<PathBuf> {
        let mut roots: Vec<PathBuf> =
            self.roots.iter().map(|r| crate::models::expand_tilde(r)).collect();
        // The FTW directory too. It defaults to `download_dir`, which is usually already a
        // root — but when it is configured somewhere else, every build paddock itself wrote
        // was invisible to the library that offered to write it, and the Models tab showed
        // a checkpoint with no conversion beside tens of gigabytes of one.
        for implied in [self.hub_cache(), self.ftw_dir()] {
            if !roots.contains(&implied) {
                roots.push(implied);
            }
        }
        roots
    }

    /// The hub cache this run reads and downloads into.
    ///
    /// Configuration first, deliberately. Everything else here is inherited from an
    /// environment that may or may not have been set up, and a tool that behaves
    /// differently depending on how its terminal was started is a tool nobody can debug.
    pub fn hub_cache(&self) -> PathBuf {
        match &self.hub_cache {
            Some(dir) => crate::models::expand_tilde(dir),
            None => hub_cache_dir(),
        }
    }

    /// Where FTW builds go.
    pub fn ftw_dir(&self) -> PathBuf {
        crate::models::expand_tilde(self.ftw_dir.as_ref().unwrap_or(&self.download_dir))
    }
}

/// Where `huggingface_hub` keeps the token `hf auth login` writes.
///
/// `HF_TOKEN_PATH`, then `$HF_HOME/token`, then the default `HF_HOME`. Hardcoding
/// `~/.cache/huggingface/token` is wrong the moment `HF_HOME` moves — which it does on any
/// machine pointing its cache at shared storage — and the failure is silent in the worst
/// way: the token is plainly on disk, paddock reports none, and gated downloads 401 for no
/// visible reason.
pub fn hf_token_path() -> PathBuf {
    if let Some(path) = std::env::var_os("HF_TOKEN_PATH") {
        return PathBuf::from(path);
    }
    if let Some(home) = std::env::var_os("HF_HOME") {
        return PathBuf::from(home).join("token");
    }
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".cache/huggingface/token")
}

/// The hub cache as the *environment* describes it.
///
/// Resolution matches `huggingface_hub`'s own — `HF_HUB_CACHE`, then `$HF_HOME/hub`, then
/// the documented default — so paddock agrees with the rest of the ecosystem when those are
/// set. Prefer [`LibraryCfg::hub_cache`], which lets configuration override all of it:
/// getting this wrong is otherwise invisible, and the library simply looks empty on a
/// machine holding hundreds of gigabytes.
pub fn hub_cache_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("HF_HUB_CACHE") {
        return PathBuf::from(dir);
    }
    if let Some(home) = std::env::var_os("HF_HOME") {
        return PathBuf::from(home).join("hub");
    }
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".cache/huggingface/hub")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HubCfg {
    /// Hugging Face endpoint; override for a mirror.
    pub endpoint: String,
    /// Access token for gated or private repos. `HF_TOKEN` in the environment wins.
    pub token: Option<String>,
    /// Concurrent file downloads, passed to `hf download --max-workers`.
    pub concurrency: usize,
    /// Glob-ish suffixes skipped by default (duplicate weight formats, mostly).
    pub ignore: Vec<String>,
    /// Explicit path to the `hf` CLI. Overrides discovery, which looks in the FreeToken
    /// venv (it ships `huggingface_hub`) before falling back to PATH.
    pub cli: Option<PathBuf>,
}

impl Default for HubCfg {
    fn default() -> Self {
        Self {
            endpoint: "https://huggingface.co".into(),
            token: None,
            cli: None,
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
#[derive(Debug, Clone, Serialize)]
pub struct HubToken {
    // Never serialized. Provenance is what the UI needs; the token itself is a secret
    // that has no business on a wire paddock does not control.
    #[serde(skip)]
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
        let raw = std::fs::read_to_string(hf_token_path()).ok()?;
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

/// The browser interface `paddock web` serves.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WebCfg {
    /// Address to bind. 7979 sits well away from the engine's 1919 and the 192x range
    /// FreeToken's own subprocesses use.
    pub listen: String,
    /// Bearer token every `/api` request must carry. Unset means no authentication at
    /// all, which is `ft serve`'s own default.
    pub token: Option<String>,
}

impl Default for WebCfg {
    fn default() -> Self {
        Self { listen: "0.0.0.0:7979".into(), token: None }
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
    /// Name of the profile selected when paddock last exited.
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
/// tests never touch the developer's real paddock configuration. Idempotent and shared by
/// every test module, since the roots are process-global.
#[cfg(test)]
pub fn isolate_paths_for_tests() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let dir = std::env::temp_dir().join(format!("paddock-tests-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("state")).unwrap();
        std::env::set_var("PADDOCK_CONFIG_DIR", &dir);
        std::env::set_var("PADDOCK_STATE_DIR", dir.join("state"));
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

    /// The rename's migration step, which decides whether an existing install survives.
    ///
    /// Exercised through `migrate_one` rather than `migrate_legacy_dirs`, because the test
    /// harness sets PADDOCK_*_DIR process-wide and the public entry point correctly
    /// declines to move anything when those are set.
    mod legacy_migration {
        use super::*;

        fn scratch(tag: &str) -> PathBuf {
            let dir = std::env::temp_dir().join(format!(
                "paddock-migrate-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }

        #[test]
        fn a_pre_rename_install_is_moved_into_place() {
            let root = scratch("move");
            let (from, to) = (root.join("ft-man"), root.join("paddock"));
            std::fs::create_dir_all(&from).unwrap();
            // serve.json is the file that matters: losing it orphans a running engine.
            std::fs::write(from.join("serve.json"), "{}").unwrap();
            std::fs::write(from.join("costs.json"), "{}").unwrap();

            migrate_one(&from, &to);

            assert!(to.join("serve.json").is_file(), "serve.json did not survive the migration");
            assert!(to.join("costs.json").is_file());
            assert!(!from.exists(), "the old directory should be gone, not copied");
        }

        #[test]
        fn an_existing_new_directory_always_wins() {
            let root = scratch("both");
            let (from, to) = (root.join("ft-man"), root.join("paddock"));
            std::fs::create_dir_all(&from).unwrap();
            std::fs::create_dir_all(&to).unwrap();
            std::fs::write(from.join("config.toml"), "old").unwrap();
            std::fs::write(to.join("config.toml"), "current").unwrap();

            migrate_one(&from, &to);

            // Overwriting here would silently roll a live configuration back.
            assert_eq!(std::fs::read_to_string(to.join("config.toml")).unwrap(), "current");
            assert!(from.is_dir(), "the old directory should be left alone, not consumed");
        }

        #[test]
        fn nothing_to_migrate_is_not_an_error() {
            let root = scratch("absent");
            migrate_one(&root.join("ft-man"), &root.join("paddock"));
            assert!(!root.join("paddock").exists());
        }
    }

    /// Serialize every test that reads or writes `HF_*`.
    ///
    /// The environment is process-global and `cargo test` runs these on separate threads,
    /// so two of them setting `HF_HOME` to different directories is a genuine race: one
    /// test's `set_var` lands between another's `set_var` and its assertion, and the
    /// failure moves around between runs. A plain `Mutex` because nothing here awaits.
    fn hf_env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        // A test that panics mid-assertion poisons the lock; the next one still has to
        // run, and there is no shared state to be left inconsistent.
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `resolve_token` reads process-global environment, so these cases run under one
    /// lock and one test rather than racing each other.
    #[test]
    fn hub_token_resolution_prefers_env_then_config() {
        let _env = hf_env_lock();
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

        // The on-disk token follows HF_HOME. Pointing a machine's cache at shared storage
        // is exactly when this used to break: `hf auth login` writes $HF_HOME/token, and
        // paddock read ~/.cache/huggingface/token and reported no token at all.
        let dir = std::env::temp_dir().join(format!("paddock-hfhome-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("token"), "hf_from_disk\n").unwrap();
        cfg.token = None;
        std::env::set_var("HF_HOME", &dir);
        let resolved = cfg.resolve_token().expect("a token under HF_HOME must be found");
        assert_eq!(resolved.value, "hf_from_disk");

        // HF_TOKEN_PATH wins over HF_HOME, as it does for huggingface_hub.
        std::fs::write(dir.join("other"), "hf_explicit").unwrap();
        std::env::set_var("HF_TOKEN_PATH", dir.join("other"));
        assert_eq!(cfg.resolve_token().unwrap().value, "hf_explicit");

        std::env::remove_var("HF_TOKEN_PATH");
        std::env::remove_var("HF_HOME");
        std::fs::remove_dir_all(&dir).ok();
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

    /// The failure this exists to prevent: `HF_HOME` is exported by a shell profile, so a
    /// session opened before that profile was written — or a systemd unit, or a terminal an
    /// editor spawned — sees none of it. paddock then read an empty cache under $HOME and
    /// reported a library of nothing on a machine holding 100 GB of weights.
    #[test]
    fn a_configured_cache_beats_the_environment() {
        let _env = hf_env_lock();
        let mut lib = LibraryCfg::default();

        // With nothing configured, the environment decides, matching huggingface_hub.
        std::env::remove_var("HF_HUB_CACHE");
        std::env::set_var("HF_HOME", "/env/hf");
        assert_eq!(lib.hub_cache(), PathBuf::from("/env/hf/hub"));
        std::env::set_var("HF_HUB_CACHE", "/env/explicit");
        assert_eq!(lib.hub_cache(), PathBuf::from("/env/explicit"));

        // Configured, it wins — that is the whole point. A tool that behaves differently
        // depending on how its terminal was started cannot be debugged.
        lib.hub_cache = Some(PathBuf::from("/workspace/huggingface/hub"));
        assert_eq!(lib.hub_cache(), PathBuf::from("/workspace/huggingface/hub"));

        // And it is scanned, whether or not it was also listed as a root.
        assert!(lib.effective_roots().contains(&PathBuf::from("/workspace/huggingface/hub")));

        // So is the FTW directory, or a build paddock wrote is one the library cannot see.
        lib.ftw_dir = Some(PathBuf::from("/fast/ftw"));
        assert!(lib.effective_roots().contains(&PathBuf::from("/fast/ftw")));
        lib.roots = vec![PathBuf::from("/fast/ftw")];
        assert_eq!(
            lib.effective_roots().iter().filter(|r| r.as_path() == Path::new("/fast/ftw")).count(),
            1,
            "listing it explicitly must not scan it twice"
        );
        lib.ftw_dir = None;
        lib.roots = LibraryCfg::default().roots;
        lib.roots = vec![PathBuf::from("/workspace/huggingface/hub")];
        assert_eq!(
            lib.effective_roots().iter().filter(|r| r.ends_with("hub")).count(),
            1,
            "listing the cache explicitly must not scan it twice"
        );

        std::env::remove_var("HF_HOME");
        std::env::remove_var("HF_HUB_CACHE");
    }
}
