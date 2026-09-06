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
use futures_util::StreamExt;
use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

// ---------------------------------------------------------------- API types

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RepoSummary {
    #[serde(rename = "id")]
    pub id: String,
    #[serde(default)]
    pub downloads: u64,
    #[serde(default)]
    pub likes: u64,
    #[serde(default, rename = "lastModified")]
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

#[derive(Debug, Clone, Default, Deserialize)]
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

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Sibling {
    #[serde(rename = "rfilename")]
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
#[derive(Debug, Clone)]
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

/// Start downloading `files` from `repo` into `target`, `concurrency` at a time.
///
/// Sizes missing from the repo listing are filled in with a HEAD first, so the progress
/// bar has a real denominator from the outset rather than growing as it goes.
pub async fn start_download(
    hub: Hub,
    repo: String,
    revision: String,
    target: PathBuf,
    files: Vec<RepoFile>,
    concurrency: usize,
    events: mpsc::UnboundedSender<DownloadEvent>,
) -> Result<Download> {
    let id = NEXT_DOWNLOAD_ID.fetch_add(1, Ordering::Relaxed);
    let mut wanted: Vec<RepoFile> = files.into_iter().filter(|f| f.wanted).collect();
    anyhow::ensure!(!wanted.is_empty(), "no files selected");

    for f in wanted.iter_mut().filter(|f| f.size == 0) {
        if let Some(size) = hub.head_size(&repo, &revision, &f.path).await {
            f.size = size;
        }
    }

    std::fs::create_dir_all(&target).with_context(|| format!("creating {}", target.display()))?;

    let total: u64 = wanted.iter().map(|f| f.size).sum();
    let done = Arc::new(AtomicU64::new(0));
    let cancel = Arc::new(AtomicBool::new(false));
    let file_count = wanted.len();

    // Count bytes already on disk from an earlier run so resuming does not restart the
    // progress bar at zero.
    for f in &wanted {
        let dest = target.join(&f.path);
        if let Ok(meta) = std::fs::metadata(&dest) {
            if meta.len() == f.size && f.size > 0 {
                done.fetch_add(f.size, Ordering::Relaxed);
            }
        }
    }

    let task = DownloadTask {
        id,
        hub: hub.clone(),
        repo: repo.clone(),
        revision: revision.clone(),
        target: target.clone(),
        done: done.clone(),
        cancel: cancel.clone(),
        events: events.clone(),
    };
    tokio::spawn(run_download(task, wanted, concurrency.max(1)));

    Ok(Download {
        id,
        repo,
        revision,
        target,
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

struct DownloadTask {
    id: u64,
    hub: Hub,
    repo: String,
    revision: String,
    target: PathBuf,
    done: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
    events: mpsc::UnboundedSender<DownloadEvent>,
}

async fn run_download(task: DownloadTask, files: Vec<RepoFile>, concurrency: usize) {
    let task = Arc::new(task);
    let results = futures_util::stream::iter(files.into_iter().map(|f| {
        let task = task.clone();
        async move {
            if task.cancel.load(Ordering::Relaxed) {
                return Ok(());
            }
            let res = fetch_file(&task, &f).await;
            if res.is_ok() {
                let _ = task.events.send(DownloadEvent::FileDone { id: task.id });
            }
            res
        }
    }))
    .buffer_unordered(concurrency)
    .collect::<Vec<Result<()>>>()
    .await;

    let outcome = if task.cancel.load(Ordering::Relaxed) {
        Err("canceled".to_string())
    } else {
        match results.into_iter().collect::<Result<Vec<_>>>() {
            Ok(_) => Ok(task.target.clone()),
            Err(e) => Err(format!("{e:#}")),
        }
    };
    let _ = task.events.send(DownloadEvent::Finished { id: task.id, result: outcome });
}

/// Fetch one file, resuming a partial `.part` if there is one.
async fn fetch_file(task: &DownloadTask, file: &RepoFile) -> Result<()> {
    let dest = task.target.join(&file.path);
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("creating {}", parent.display()))?;
    }

    // Already complete from an earlier run.
    if let Ok(meta) = tokio::fs::metadata(&dest).await {
        if file.size > 0 && meta.len() == file.size {
            return Ok(());
        }
    }

    let part = dest.with_extension(format!(
        "{}part",
        dest.extension().map(|e| format!("{}.", e.to_string_lossy())).unwrap_or_default()
    ));
    let resume_from = tokio::fs::metadata(&part).await.map(|m| m.len()).unwrap_or(0);

    let url = task.hub.resolve_url(&task.repo, &task.revision, &file.path);
    let mut req = task.hub.http.get(&url);
    if let Some(t) = &task.hub.token {
        req = req.bearer_auth(t);
    }
    if resume_from > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={resume_from}-"));
    }

    let resp = req
        .timeout(Duration::from_secs(60 * 60 * 6))
        .send()
        .await
        .with_context(|| format!("downloading {}", file.path))?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("{}: HTTP {status}", file.path);
    }
    // A server that ignored the Range header sends 200 and the whole body; restart the
    // part file rather than appending a second copy onto the first.
    let appending = resume_from > 0 && status == reqwest::StatusCode::PARTIAL_CONTENT;
    if resume_from > 0 && appending {
        task.done.fetch_add(resume_from, Ordering::Relaxed);
    }

    let mut out = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(appending)
        .truncate(!appending)
        .open(&part)
        .await
        .with_context(|| format!("opening {}", part.display()))?;

    let _ = task.events.send(DownloadEvent::Progress { id: task.id, current: file.path.clone() });

    let mut stream = resp.bytes_stream();
    let mut since_report = 0u64;
    while let Some(chunk) = stream.next().await {
        if task.cancel.load(Ordering::Relaxed) {
            out.flush().await.ok();
            anyhow::bail!("canceled");
        }
        let chunk = chunk.with_context(|| format!("reading {}", file.path))?;
        out.write_all(&chunk).await.with_context(|| format!("writing {}", part.display()))?;
        let n = chunk.len() as u64;
        task.done.fetch_add(n, Ordering::Relaxed);
        since_report += n;
        if since_report >= 1 << 20 {
            since_report = 0;
            let _ = task
                .events
                .send(DownloadEvent::Progress { id: task.id, current: file.path.clone() });
        }
    }
    out.flush().await.ok();
    drop(out);

    tokio::fs::rename(&part, &dest)
        .await
        .with_context(|| format!("finalizing {}", dest.display()))?;
    Ok(())
}

/// Where a repo lands by default: `<download_dir>/<repo basename>`.
pub fn default_target(download_dir: &Path, repo: &str) -> PathBuf {
    let name = repo.rsplit('/').next().unwrap_or(repo);
    crate::models::expand_tilde(download_dir).join(name)
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

    #[test]
    fn target_uses_the_repo_basename() {
        assert_eq!(
            default_target(Path::new("/models"), "Qwen/Qwen3.6-35B-A3B"),
            PathBuf::from("/models/Qwen3.6-35B-A3B")
        );
    }
}
