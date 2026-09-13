//! Hugging Face Hub: search, inspect, and download checkpoints.
//!
//! Implemented directly against the Hub's HTTP API rather than shelling out to `hf`, so
//! ft-man has no Python dependency of its own and can render real per-file progress.
//! Downloads are resumable (`Range` on an existing `.part` file), run several files at a
//! time, and land in the same layout `hf download --local-dir` produces, so a checkpoint
//! fetched here is interchangeable with one fetched by the official tool.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

// ---------------------------------------------------------------- API types

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RepoSummary {
    #[serde(rename = "id")]
    pub id: String,
    #[serde(default)]
    pub downloads: u64,
    #[serde(default)]
    pub likes: u64,
    #[serde(default, rename(deserialize = "lastModified"))]
    pub last_modified: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub gated: serde_json::Value,
    #[serde(default)]
    pub private: bool,
}

impl RepoSummary {
    pub fn is_gated(&self) -> bool {
        !matches!(self.gated, serde_json::Value::Bool(false) | serde_json::Value::Null)
    }

    /// Tags that say something useful in a narrow list: parameter count, quantization,
    /// architecture. The Hub attaches dozens; most are noise here.
    pub fn interesting_tags(&self) -> Vec<&str> {
        const PREFIXES: [&str; 4] = ["base_model:", "license:", "region:", "arxiv:"];
        self.tags
            .iter()
            .map(String::as_str)
            .filter(|t| !PREFIXES.iter().any(|p| t.starts_with(p)))
            .take(6)
            .collect()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RepoInfo {
    #[serde(rename = "id")]
    pub id: String,
    /// The commit the requested revision resolved to.
    #[serde(default)]
    pub sha: Option<String>,
    #[serde(default)]
    pub gated: serde_json::Value,
    #[serde(default)]
    pub siblings: Vec<Sibling>,
}

impl RepoInfo {
    pub fn is_gated(&self) -> bool {
        !matches!(self.gated, serde_json::Value::Bool(false) | serde_json::Value::Null)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Sibling {
    #[serde(rename(deserialize = "rfilename"))]
    pub path: String,
    #[serde(default)]
    pub size: Option<u64>,
}

/// The `.jinja` files in a repo listing, which is what a chat-template repo holds.
pub fn jinja_files(siblings: &[Sibling]) -> Vec<Sibling> {
    let mut out: Vec<Sibling> =
        siblings.iter().filter(|s| s.path.ends_with(".jinja")).cloned().collect();
    // Repo-root templates first: that is the current one, with archives beneath it.
    out.sort_by_key(|s| (s.path.contains('/'), s.path.clone()));
    out
}

/// A file resolved for download: path within the repo plus its size.
#[derive(Debug, Clone, Serialize)]
pub struct RepoFile {
    pub path: String,
    pub size: u64,
    /// Whether it is selected for download.
    pub wanted: bool,
}

// ---------------------------------------------------------------- client

#[derive(Clone)]
pub struct Hub {
    http: reqwest::Client,
    endpoint: String,
    token: Option<String>,
}

impl Hub {
    pub fn new(endpoint: &str, token: Option<String>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .user_agent(concat!("ft-man/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("building the Hub HTTP client")?;
        Ok(Self { http, endpoint: endpoint.trim_end_matches('/').to_string(), token })
    }

    pub fn has_token(&self) -> bool {
        self.token.is_some()
    }

    fn authed(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.token {
            Some(t) => req.bearer_auth(t),
            None => req,
        }
    }

    /// Full-text model search, newest-and-most-downloaded first.
    pub async fn search(&self, query: &str, limit: u32) -> Result<Vec<RepoSummary>> {
        let url = format!("{}/api/models", self.endpoint);
        let req = self.http.get(&url).query(&[
            ("search", query),
            ("limit", &limit.to_string()),
            ("sort", "downloads"),
            ("direction", "-1"),
            ("full", "false"),
        ]);
        let resp = self.authed(req).send().await.context("searching the Hub")?;
        decode(resp, "model search").await
    }

    /// Full repo metadata including the file listing.
    pub async fn info(&self, repo: &str, revision: &str) -> Result<RepoInfo> {
        let url = format!("{}/api/models/{}/revision/{}", self.endpoint, repo, urlencode(revision));
        let resp = self
            .authed(self.http.get(&url).query(&[("blobs", "true")]))
            .send()
            .await
            .with_context(|| format!("fetching metadata for {repo}"))?;
        decode(resp, repo).await
    }

    fn resolve_url(&self, repo: &str, revision: &str, path: &str) -> String {
        format!("{}/{}/resolve/{}/{}", self.endpoint, repo, urlencode(revision), path)
    }

    /// Download one small file straight into memory. Templates are tens of kilobytes,
    /// so they need none of the resumable, chunked machinery the weight files do.
    pub async fn fetch_text(&self, repo: &str, revision: &str, path: &str) -> Result<String> {
        let url = self.resolve_url(repo, revision, path);
        let resp = self
            .authed(self.http.get(&url))
            .send()
            .await
            .with_context(|| format!("fetching {path}"))?;
        let status = resp.status();
        let text = resp.text().await.with_context(|| format!("reading {path}"))?;
        if !status.is_success() {
            anyhow::bail!("{path}: HTTP {status}");
        }
        // A repo can be large; a template that arrives as tens of megabytes is a sign
        // something other than a template was requested.
        anyhow::ensure!(
            text.len() <= 4 << 20,
            "{path} is {} — too large to be a chat template",
            crate::util::bytes(text.len() as u64)
        );
        Ok(text)
    }

    /// HEAD a file to learn its size when the listing did not carry one.
    async fn head_size(&self, repo: &str, revision: &str, path: &str) -> Option<u64> {
        let url = self.resolve_url(repo, revision, path);
        let resp = self.authed(self.http.head(&url)).send().await.ok()?;
        // The CDN redirect carries the real size in this header.
        resp.headers()
            .get("x-linked-size")
            .or_else(|| resp.headers().get(reqwest::header::CONTENT_LENGTH))
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
    }
}

async fn decode<T: serde::de::DeserializeOwned>(resp: reqwest::Response, what: &str) -> Result<T> {
    let status = resp.status();
    let bytes = resp.bytes().await.with_context(|| format!("reading {what}"))?;
    if !status.is_success() {
        let body: String = String::from_utf8_lossy(&bytes).chars().take(200).collect();
        anyhow::bail!("{what}: {status} {body}");
    }
    serde_json::from_slice(&bytes).with_context(|| format!("decoding {what}"))
}

/// Percent-encode a path segment. Revisions can be branch names with slashes, which the
/// Hub expects encoded.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ---------------------------------------------------------------- file selection

/// Decide which files to pull. Weights, tokenizer and config are wanted; alternate
/// serialization formats are not, because a repo that ships both safetensors and `.bin`
/// would otherwise double the download for nothing.
///
/// `keep_gguf` is separate because GGUF is a *usable* format for FreeToken (Gemma-4),
/// not a redundant one — but pulling it alongside safetensors is still waste, so it is
/// only kept when the repo has no safetensors.
pub fn select_files(siblings: &[Sibling], ignore: &[String]) -> Vec<RepoFile> {
    let has_safetensors = siblings.iter().any(|s| s.path.ends_with(".safetensors"));
    let mut files: Vec<RepoFile> = siblings
        .iter()
        .filter(|s| !s.path.ends_with('/'))
        .map(|s| {
            let wanted = wanted_by_default(&s.path, ignore, has_safetensors);
            RepoFile { path: s.path.clone(), size: s.size.unwrap_or(0), wanted }
        })
        .collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files
}

fn wanted_by_default(path: &str, ignore: &[String], has_safetensors: bool) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    if name.starts_with('.') {
        return false;
    }
    for pattern in ignore {
        if glob_match(pattern, name) {
            return false;
        }
    }
    if name.ends_with(".gguf") && has_safetensors {
        return false;
    }
    // Consolidated mirrors of the sharded weights: pure duplication.
    if name.starts_with("consolidated") {
        return false;
    }
    // Skip the extras a serving engine never reads.
    const SKIP_DIRS: [&str; 4] = ["onnx/", "coreml/", "openvino/", "tflite/"];
    if SKIP_DIRS.iter().any(|d| path.starts_with(d)) {
        return false;
    }
    true
}

/// A deliberately small glob: `*` at either end, literal otherwise. That covers the
/// `*.bin` shapes the ignore list actually uses without pulling in a glob crate.
fn glob_match(pattern: &str, name: &str) -> bool {
    match (pattern.strip_prefix('*'), pattern.strip_suffix('*')) {
        (Some(suffix), None) => name.ends_with(suffix),
        (None, Some(prefix)) => name.starts_with(prefix),
        (Some(_), Some(_)) => {
            let inner = pattern.trim_matches('*');
            inner.is_empty() || name.contains(inner)
        }
        (None, None) => pattern == name,
    }
}

// ---------------------------------------------------------------- download

#[derive(Debug)]
pub enum DownloadEvent {
    /// The file that just started transferring.
    Progress {
        id: u64,
        current: String,
    },
    /// One file finished; the UI keeps its own completed count.
    FileDone {
        id: u64,
    },
    Finished {
        id: u64,
        result: Result<PathBuf, String>,
    },
}

/// Shared, cancellable state for one repo download.
#[derive(Debug)]
pub struct Download {
    pub id: u64,
    pub repo: String,
    pub revision: String,
    pub target: PathBuf,
    pub total_bytes: u64,
    pub done_bytes: Arc<AtomicU64>,
    pub file_count: usize,
    pub files_done: usize,
    pub current: String,
    pub status: DownloadStatus,
    pub started_at: chrono::DateTime<chrono::Local>,
    pub finished_at: Option<chrono::DateTime<chrono::Local>>,
    pub rate: crate::util::Ema,
    last_sample: (std::time::Instant, u64),
    cancel: Arc<AtomicBool>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DownloadStatus {
    Running,
    Done,
    Failed(String),
    Canceled,
}

// Written out for the same reason as `JobStatus`: serde cannot internally tag a newtype
// variant holding a plain String, and the wire format names that payload `reason`.
impl Serialize for DownloadStatus {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = s.serialize_map(None)?;
        match self {
            DownloadStatus::Running => m.serialize_entry("kind", "running")?,
            DownloadStatus::Done => m.serialize_entry("kind", "done")?,
            DownloadStatus::Canceled => m.serialize_entry("kind", "canceled")?,
            DownloadStatus::Failed(reason) => {
                m.serialize_entry("kind", "failed")?;
                m.serialize_entry("reason", reason)?;
            }
        }
        m.end()
    }
}

impl Download {
    pub fn is_running(&self) -> bool {
        self.status == DownloadStatus::Running
    }

    pub fn cancel(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.status = DownloadStatus::Canceled;
        self.finished_at = Some(chrono::Local::now());
    }

    pub fn done(&self) -> u64 {
        self.done_bytes.load(Ordering::Relaxed)
    }

    pub fn ratio(&self) -> f64 {
        crate::util::ratio(self.done(), self.total_bytes)
    }

    /// Recompute the smoothed transfer rate. Called once per UI tick so the sample
    /// interval is regular.
    pub fn sample_rate(&mut self) -> f64 {
        let now = std::time::Instant::now();
        let done = self.done();
        let dt = now.duration_since(self.last_sample.0).as_secs_f64();
        if dt >= 0.25 {
            let delta = done.saturating_sub(self.last_sample.1) as f64;
            self.rate.push(delta / dt);
            self.last_sample = (now, done);
        }
        self.rate.get()
    }

    pub fn elapsed(&self) -> Duration {
        let end = self.finished_at.unwrap_or_else(chrono::Local::now);
        (end - self.started_at).to_std().unwrap_or_default()
    }
}

impl Download {
    /// A download in an arbitrary state, for tests. Starts no transfer.
    #[cfg(test)]
    pub fn fake(repo: &str, total: u64, done: u64, status: DownloadStatus) -> Self {
        Self {
            id: NEXT_DOWNLOAD_ID.fetch_add(1, Ordering::Relaxed),
            repo: repo.into(),
            revision: "main".into(),
            target: PathBuf::from("/models/x"),
            total_bytes: total,
            done_bytes: Arc::new(AtomicU64::new(done)),
            file_count: 12,
            files_done: 3,
            current: "model-00003-of-00012.safetensors".into(),
            status,
            started_at: chrono::Local::now(),
            finished_at: None,
            rate: crate::util::Ema::new(0.3),
            last_sample: (std::time::Instant::now(), 0),
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }
}

static NEXT_DOWNLOAD_ID: AtomicU64 = AtomicU64::new(1);

/// Locate the `hf` CLI.
///
/// Order: an explicit `hub.cli`, then the FreeToken venv — FreeToken depends on
/// `huggingface_hub`, so `hf` is installed right beside `ft` — then beside an explicitly
/// configured `ft` binary, then PATH.
pub fn locate_cli(
    cfg: &crate::config::Config,
    ft_program: Option<&Path>,
) -> Result<PathBuf, String> {
    if let Some(p) = &cfg.hub.cli {
        let p = crate::models::expand_tilde(p);
        if crate::ft::locate::is_executable(&p) {
            return Ok(p);
        }
        return Err(format!("hub.cli is set to {}, which is not an executable", p.display()));
    }
    if let Some(venv) = &cfg.freetoken.venv {
        let hf = crate::models::expand_tilde(venv).join("bin/hf");
        if crate::ft::locate::is_executable(&hf) {
            return Ok(hf);
        }
    }
    if let Some(sibling) = ft_program.and_then(|p| p.parent()).map(|d| d.join("hf")) {
        if crate::ft::locate::is_executable(&sibling) {
            return Ok(sibling);
        }
    }
    if let Some(p) = crate::ft::locate::which("hf") {
        return Ok(p);
    }
    Err(format!(
        "could not find the `hf` CLI (looked in the FreeToken venv, beside the `ft` binary, and \
         on PATH). It ships with huggingface_hub; set hub.cli in {} to point at it.",
        crate::config::config_path().display()
    ))
}

/// Hugging Face's own installer for the `hf` CLI, exactly as their CLI guide documents it
/// under "Standalone installer (Recommended)".
///
/// Deliberately not a hand-rolled `pip install` into some virtualenv ft-man picked: which
/// environment a CLI belongs in is the packager's decision, not this tool's, and an
/// invented install path is one more thing to be wrong about when it moves.
///
/// `--exclude-skill` is theirs too. The installer otherwise also writes agent skills into
/// `~/.agents/skills`, which is a surprising thing for "ft-man could not find a downloader"
/// to do to someone's home directory.
pub const INSTALL_COMMAND: &str =
    "curl -LsSf https://hf.co/cli/install.sh | bash -s -- --exclude-skill";

/// Run the documented installer, returning where `hf` ended up.
///
/// The script installs into `~/.local/bin`, which is not necessarily on the PATH ft-man
/// inherited, so the result is looked for there explicitly rather than trusting a
/// subsequent PATH lookup to find it.
pub async fn install_cli() -> Result<PathBuf, String> {
    let out = tokio::process::Command::new("bash")
        .arg("-c")
        .arg(INSTALL_COMMAND)
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map_err(|e| format!("could not run the installer: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "the installer exited {}: {}",
            out.status.code().unwrap_or(-1),
            last_line(&err)
        ));
    }
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    for candidate in [home.join(".local/bin/hf"), home.join(".cargo/bin/hf")] {
        if crate::ft::locate::is_executable(&candidate) {
            return Ok(candidate);
        }
    }
    crate::ft::locate::which("hf").ok_or_else(|| {
        "the installer reported success but `hf` is still not on PATH; open a new shell and \
         restart ft-man, or set hub.cli in the config"
            .to_string()
    })
}

/// The cache directory a repo occupies: `<cache>/models--org--name`.
pub fn cache_repo_dir(cache: &Path, repo: &str) -> PathBuf {
    cache.join(format!("models--{}", repo.replace('/', "--")))
}

/// Start downloading `files` from `repo` into the Hugging Face hub cache.
///
/// The transfer is delegated to `hf download` rather than reimplemented. The cache layout
/// is not merely a directory naming scheme — it is blobs addressed by hash, a snapshot of
/// symlinks per revision, refs, and `.incomplete` staging — and a second implementation of
/// it that is subtly wrong yields a cache every other tool quietly disagrees with. `hf` is
/// the reference implementation, and it is already present because FreeToken depends on
/// `huggingface_hub`.
///
/// Progress stays real, and stays in bytes. `hf` reports only a file count on stderr
/// (`Fetching 12 files:  25%`), which on a repo of two 40 GiB shards is a bar that sits at
/// zero for an hour and then jumps to done. So the byte count is observed from the cache
/// instead: files already resolved through the snapshot, plus the `.incomplete` blobs
/// `huggingface_hub` stages an in-flight file in.
#[allow(clippy::too_many_arguments)]
pub async fn start_download(
    hub: Hub,
    cli: PathBuf,
    cache_dir: PathBuf,
    token: Option<String>,
    repo: String,
    revision: String,
    files: Vec<RepoFile>,
    concurrency: usize,
    events: mpsc::UnboundedSender<DownloadEvent>,
) -> Result<Download> {
    let id = NEXT_DOWNLOAD_ID.fetch_add(1, Ordering::Relaxed);
    let mut wanted: Vec<RepoFile> = files.into_iter().filter(|f| f.wanted).collect();
    anyhow::ensure!(!wanted.is_empty(), "no files selected");

    // `hf` moves the bytes, but the denominator is still ft-man's problem: a size missing
    // from the repo listing is filled in with a HEAD, so the bar starts against a real
    // total rather than one that grows as it goes.
    for f in wanted.iter_mut().filter(|f| f.size == 0) {
        if let Some(size) = hub.head_size(&repo, &revision, &f.path).await {
            f.size = size;
        }
    }

    let repo_dir = cache_repo_dir(&cache_dir, &repo);
    let total: u64 = wanted.iter().map(|f| f.size).sum();
    let file_count = wanted.len();
    let start = sample(&repo_dir, &wanted, total);
    let done = Arc::new(AtomicU64::new(start.bytes));
    let cancel = Arc::new(AtomicBool::new(false));

    let mut cmd = tokio::process::Command::new(&cli);
    cmd.arg("download")
        .arg(&repo)
        .arg("--revision")
        .arg(&revision)
        .arg("--max-workers")
        .arg(concurrency.max(1).to_string())
        .arg("--format")
        .arg("json");
    // Exact filenames rather than `--include` globs: the selection is already exact, and a
    // glob would have to be escaped for filenames containing metacharacters.
    for f in &wanted {
        cmd.arg(&f.path);
    }
    cmd.env("HF_HUB_CACHE", &cache_dir)
        // Only read for diagnostics, so the bars are noise that would interleave with the
        // one line that matters when a download fails.
        .env("HF_HUB_DISABLE_PROGRESS_BARS", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // In the environment, never as `--token`: an argument is visible to every user on the
    // machine in `ps`, and this one is a credential.
    if let Some(t) = token {
        cmd.env("HF_TOKEN", t);
    }

    let child = cmd.spawn().with_context(|| format!("running {}", cli.display()))?;

    tokio::spawn(run_download(DownloadWatch {
        id,
        child,
        repo_dir: repo_dir.clone(),
        wanted,
        total,
        files_done: start.files_done,
        done: done.clone(),
        cancel: cancel.clone(),
        events: events.clone(),
    }));

    Ok(Download {
        id,
        repo,
        revision,
        target: repo_dir,
        total_bytes: total,
        done_bytes: done,
        file_count,
        files_done: 0,
        current: String::new(),
        status: DownloadStatus::Running,
        started_at: chrono::Local::now(),
        finished_at: None,
        rate: crate::util::Ema::new(0.3),
        last_sample: (std::time::Instant::now(), 0),
        cancel,
    })
}

struct DownloadWatch {
    id: u64,
    child: tokio::process::Child,
    repo_dir: PathBuf,
    wanted: Vec<RepoFile>,
    total: u64,
    files_done: usize,
    done: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
    events: mpsc::UnboundedSender<DownloadEvent>,
}

/// Supervise one `hf download`, sampling the cache for byte progress until it exits.
async fn run_download(mut watch: DownloadWatch) {
    let stdout = watch.child.stdout.take();
    let stderr = watch.child.stderr.take();
    let mut pending = String::new();

    let mut ticker = tokio::time::interval(Duration::from_millis(500));
    let status = loop {
        tokio::select! {
            res = watch.child.wait() => break res,
            _ = ticker.tick() => {
                if watch.cancel.load(Ordering::Relaxed) {
                    // The child owns the partial `.incomplete` blobs. Killing it leaves
                    // them in place, which is exactly what the next run resumes from.
                    let _ = watch.child.start_kill();
                    let _ = watch.child.wait().await;
                    let _ = watch.events.send(DownloadEvent::Finished {
                        id: watch.id,
                        result: Err("canceled".into()),
                    });
                    return;
                }
                let now = sample(&watch.repo_dir, &watch.wanted, watch.total);
                // Monotonic on purpose: completing a blob is a rename, and for an instant
                // it is counted by neither the snapshot pass nor the `.incomplete` sweep.
                if now.bytes > watch.done.load(Ordering::Relaxed) {
                    watch.done.store(now.bytes, Ordering::Relaxed);
                }
                for _ in watch.files_done..now.files_done {
                    let _ = watch.events.send(DownloadEvent::FileDone { id: watch.id });
                }
                watch.files_done = watch.files_done.max(now.files_done);
                // `hf` runs several workers at once, so there is no single current file.
                // Naming the first one still outstanding is approximate and says more than
                // a blank field.
                if now.pending != pending {
                    pending = now.pending.clone();
                    let _ = watch.events.send(DownloadEvent::Progress {
                        id: watch.id,
                        current: now.pending,
                    });
                }
            }
        }
    };

    let out = read_all(stdout).await;
    let err = read_all(stderr).await;

    let result = match status {
        Ok(s) if s.success() => {
            watch.done.store(watch.total, Ordering::Relaxed);
            // `--format json` prints `{"path": "<snapshot dir>"}` — the revision actually
            // materialized, which is what the rest of ft-man needs to act on.
            match serde_json::from_str::<DownloadReport>(out.trim()) {
                Ok(r) => Ok(PathBuf::from(r.path)),
                Err(_) => Ok(watch.repo_dir.clone()),
            }
        }
        Ok(s) => Err(format!("hf download exited {}: {}", s.code().unwrap_or(-1), last_line(&err))),
        Err(e) => Err(format!("hf download: {e}")),
    };
    let _ = watch.events.send(DownloadEvent::Finished { id: watch.id, result });
}

#[derive(Debug, Deserialize)]
struct DownloadReport {
    path: String,
}

async fn read_all<R: tokio::io::AsyncRead + Unpin>(reader: Option<R>) -> String {
    use tokio::io::AsyncReadExt;
    let Some(mut r) = reader else { return String::new() };
    let mut buf = String::new();
    let _ = r.read_to_string(&mut buf).await;
    buf
}

/// The most informative line of a failure: `hf` puts the reason last.
fn last_line(text: &str) -> String {
    text.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or("no output").to_string()
}

/// What the cache says about a download in progress.
struct Sample {
    bytes: u64,
    files_done: usize,
    /// The first wanted file not yet complete, or empty when none remain.
    pending: String,
}

/// Measure how much of `wanted` is already in the cache.
///
/// Completed files are counted through the snapshot, so a file cached before this download
/// counts immediately and a resumed transfer does not restart the bar at zero. In-flight
/// bytes come from the `.incomplete` blobs `huggingface_hub` stages each file in.
fn sample(repo_dir: &Path, wanted: &[RepoFile], total: u64) -> Sample {
    let snapshot = crate::models::cache_snapshot(repo_dir);
    let mut bytes = 0u64;
    let mut files_done = 0usize;
    let mut pending = String::new();
    for f in wanted {
        // Follows the symlink into `blobs/` deliberately: the link itself is a few bytes.
        let complete = snapshot
            .as_ref()
            .and_then(|s| std::fs::metadata(s.join(&f.path)).ok())
            .is_some_and(|m| m.len() == f.size && f.size > 0);
        if complete {
            bytes += f.size;
            files_done += 1;
        } else if pending.is_empty() {
            pending = f.path.clone();
        }
    }
    if let Ok(rd) = std::fs::read_dir(repo_dir.join("blobs")) {
        bytes += rd
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "incomplete"))
            .filter_map(|e| e.metadata().ok())
            .map(|m| m.len())
            .sum::<u64>();
    }
    Sample { bytes: bytes.min(total), files_done, pending }
}

/// Free bytes on the filesystem holding `path`, walking up to the nearest existing
/// ancestor so a not-yet-created target directory still reports something useful.
pub fn disk_free(path: &str) -> Option<u64> {
    disk_free_at(path).map(|(_, free)| free)
}

/// As [`disk_free`], but also returns the existing directory that was actually measured.
///
/// Worth surfacing: the walk up to an existing ancestor means a wrong target silently
/// reports a completely different filesystem. A cache path misresolved to the home
/// directory measured a 32 GB container root and read as "18 GB free" beside a 310 GB
/// dataset, with nothing on screen to say which one it meant.
pub fn disk_free_at(path: &str) -> Option<(PathBuf, u64)> {
    let mut p = Path::new(path);
    loop {
        if p.exists() {
            break;
        }
        p = p.parent()?;
    }
    let measured = p.to_path_buf();
    let c = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    (unsafe { libc::statvfs(c.as_ptr(), &mut stat) } == 0)
        .then(|| (measured, stat.f_bavail as u64 * stat.f_frsize as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sib(path: &str, size: u64) -> Sibling {
        Sibling { path: path.into(), size: Some(size) }
    }

    #[test]
    fn duplicate_weight_formats_are_deselected() {
        let ignore = vec!["*.bin".to_string(), "*.h5".to_string()];
        let files = select_files(
            &[
                sib("model-00001-of-00002.safetensors", 100),
                sib("pytorch_model.bin", 100),
                sib("tf_model.h5", 100),
                sib("config.json", 10),
                sib("tokenizer.json", 10),
            ],
            &ignore,
        );
        let wanted: Vec<&str> =
            files.iter().filter(|f| f.wanted).map(|f| f.path.as_str()).collect();
        assert_eq!(
            wanted,
            vec!["config.json", "model-00001-of-00002.safetensors", "tokenizer.json"]
        );
    }

    #[test]
    fn gguf_is_kept_only_when_there_are_no_safetensors() {
        let with = select_files(&[sib("model.safetensors", 1), sib("model.gguf", 1)], &[]);
        assert!(!with.iter().find(|f| f.path.ends_with(".gguf")).unwrap().wanted);

        let without = select_files(&[sib("model.gguf", 1)], &[]);
        assert!(without[0].wanted);
    }

    #[test]
    fn export_subdirectories_are_skipped() {
        let files = select_files(&[sib("onnx/model.onnx", 1), sib("config.json", 1)], &[]);
        assert!(!files.iter().find(|f| f.path.starts_with("onnx/")).unwrap().wanted);
    }

    #[test]
    fn globs_match_at_either_end() {
        assert!(glob_match("*.bin", "pytorch_model.bin"));
        assert!(!glob_match("*.bin", "model.safetensors"));
        assert!(glob_match("consolidated*", "consolidated.00.pth"));
        assert!(glob_match("config.json", "config.json"));
    }

    #[test]
    fn revisions_with_slashes_are_encoded() {
        assert_eq!(urlencode("refs/pr/3"), "refs%2Fpr%2F3");
        assert_eq!(urlencode("main"), "main");
    }

    /// The cache keys on organization *and* name. The scheme this replaced used only the
    /// basename, so `unsloth/Qwen3-GGUF` and `bartowski/Qwen3-GGUF` landed in one
    /// directory and silently merged.
    #[test]
    fn a_repo_maps_to_an_org_qualified_cache_directory() {
        assert_eq!(
            cache_repo_dir(Path::new("/hf/hub"), "Qwen/Qwen3.6-35B-A3B"),
            PathBuf::from("/hf/hub/models--Qwen--Qwen3.6-35B-A3B")
        );
        assert_ne!(
            cache_repo_dir(Path::new("/hf/hub"), "unsloth/Qwen3-GGUF"),
            cache_repo_dir(Path::new("/hf/hub"), "bartowski/Qwen3-GGUF")
        );
    }
}
