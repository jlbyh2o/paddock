//! The local model library: what is on disk, what format it is in, and how big it is.
//!
//! FreeToken serves two shapes of checkpoint — a Hugging Face directory of safetensors,
//! and its own FTW fast-load directory — and the whole convert workflow is about turning
//! the first into the second. So the scanner's job is to recognize both, pair them up
//! where a conversion already exists, and surface enough metadata (architecture, MoE-ness,
//! quantization, context length) to decide what to do next without opening a JSON file by
//! hand.
//!
//! Checkpoints are found in three layouts. A plain directory, the `org/model` layout that
//! `hf download --local-dir` and mirrors produce, and — the one that matters most on a
//! machine shared with other engines — the Hugging Face **hub cache**,
//! `models--org--name/snapshots/<sha>/`. The cache is where `from_pretrained`, `hf
//! download` and every library built on `huggingface_hub` already put weights, so reading
//! it is what lets ft-man see a model Ollama or Unsloth downloaded, and vice versa.
//!
//! The cache is read, never written. Anything ft-man derives — an FTW build, a chat
//! template override — goes elsewhere, because that tree belongs to `huggingface_hub`:
//! a directory it did not write is invisible to `hf cache scan` and at risk from `hf cache
//! delete`, and an FTW build has no repo id or revision for the cache to file it under.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    /// A Hugging Face directory: `config.json` plus safetensors shards.
    Hf,
    /// A converted FreeToken Weight directory.
    Ftw,
    /// A GGUF file or a directory containing one (Gemma-4 loads these natively).
    Gguf,
    /// Shard files but no index: a conversion that died partway. Useless to serve and
    /// potentially enormous, so it is listed rather than hidden — an unlisted directory
    /// is one nothing can delete.
    PartialFtw,
}

impl Format {
    pub fn label(self) -> &'static str {
        match self {
            Format::Hf => "HF",
            Format::Ftw => "FTW",
            Format::Gguf => "GGUF",
            Format::PartialFtw => "PART",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Model {
    pub name: String,
    /// The Hugging Face repo id (`org/name`) when this checkpoint came from the hub
    /// cache. `None` for a plain directory, which carries no repo identity — and the
    /// distinction matters, because `ft serve --model-path` accepts a repo id directly.
    pub repo: Option<String>,
    /// Which quantization of [`Self::repo`] this is, when the repo ships more than one.
    /// `UD-IQ3_XXS`, `Q8_0`. `None` for a repo with a single build.
    pub variant: Option<String>,
    pub path: PathBuf,
    pub format: Format,
    /// Total bytes of weight files under the directory.
    pub size_bytes: u64,
    pub arch: Option<String>,
    pub model_type: Option<String>,
    /// True when the config declares experts.
    pub is_moe: bool,
    pub num_experts: Option<u64>,
    pub num_layers: Option<u64>,
    /// Quantization as declared by `quantization_config.quant_method`, or FTW's
    /// `quant_format`.
    pub quant: Option<String>,
    /// `max_position_embeddings`, the ceiling on context length.
    pub max_position: Option<u64>,
    /// For an FTW directory: what it was converted from, if recorded.
    pub ftw_fingerprint: Option<String>,
    /// An FTW sibling built from this checkpoint, when the scan found one.
    pub converted_to: Option<PathBuf>,
    /// Directory mtime, shown so a freshly downloaded checkpoint is identifiable.
    // Skipped: a `SystemTime` serializes as a struct of seconds and nanoseconds, which no
    // browser wants. The web layer sends `modified_ms` beside the flattened rest.
    #[serde(skip)]
    pub modified: Option<std::time::SystemTime>,
    /// The chat-template situation as of the scan, read here rather than per frame.
    ///
    /// Both front ends want it for every row, and the web daemon builds its snapshot under
    /// the `App` mutex on the async runtime — where a `stat` per model per frame against a
    /// network mount stalls every connected browser. `App::refresh_template_status` puts it
    /// back in step after an apply or a revert, which are the only things that change it.
    // Skipped: the web layer sends it under its own `template_status` key, beside the
    // flattened rest, so the shape stays the documented one.
    #[serde(skip)]
    pub template_status: crate::templates::Status,
    /// The sampling-override situation as of the scan, read here for the same reason as
    /// `template_status`: both front ends want it per row, and a `stat` per model per
    /// frame against a network mount stalls every connected browser.
    // Skipped: the web layer sends it under its own `sampling_status` key.
    #[serde(skip)]
    pub sampling_status: crate::sampling::Status,
    /// What this checkpoint will actually hand the engine, override or not. Read at scan
    /// time for the same reason as the status beside it: it is a file read, and one per
    /// model per frame is what stalls a browser on a network mount.
    #[serde(skip)]
    pub sampling_effective: Option<crate::sampling::Sampling>,
    /// Whether `inference/config.json` is present — DeepSeek-V4 keeps its real arguments
    /// there, and the Models pane says so when it is missing. Recorded at scan time for
    /// the same reason as `template_status`.
    #[serde(skip)]
    pub has_inference_config: bool,
}

impl Model {
    /// Short one-line description used in list rows.
    pub fn summary(&self) -> String {
        if self.is_partial() {
            return "incomplete conversion — safe to delete".into();
        }
        let mut parts: Vec<String> = Vec::new();
        if let Some(a) = &self.arch {
            parts.push(a.clone());
        }
        if self.is_moe {
            parts.push(match self.num_experts {
                Some(n) => format!("MoE x{n}"),
                None => "MoE".into(),
            });
        }
        if let Some(q) = &self.quant {
            parts.push(q.to_uppercase());
        }
        if let Some(ctx) = self.max_position {
            parts.push(format!("{}k ctx", ctx / 1024));
        }
        parts.join(" · ")
    }

    /// The name to serve this model under, and the key its measured cache costs are
    /// remembered by.
    ///
    /// Always passed explicitly, because FreeToken's own default is
    /// `os.path.basename(model_path)` — which for a Hugging Face snapshot is a 40-character
    /// commit sha, and for a quantization subdirectory is a bare `UD-IQ3_XXS` that says
    /// nothing about which model it is. Both are unusable as an API model id, and both
    /// would poison `costs.json`, which is keyed by this name.
    pub fn served_name(&self) -> String {
        let base = self.repo.clone().unwrap_or_else(|| self.name.clone());
        match &self.variant {
            Some(v) => format!("{base}:{v}"),
            None => base,
        }
    }

    /// Whether converting this checkpoint to FTW is a sensible next action.
    pub fn convertible(&self) -> bool {
        self.format == Format::Hf
    }

    /// True when this is the wreckage of a failed conversion.
    pub fn is_partial(&self) -> bool {
        self.format == Format::PartialFtw
    }
}

/// Scan every configured root and return the library, sorted by name.
///
/// Three layouts are recognized per root. A hub cache entry (`models--org--name`) is
/// resolved through its ref to the snapshot it points at. Otherwise the root is searched
/// at depth 1 and 2: depth 1 catches `~/models/Qwen3.6-35B-A3B`, and depth 2 catches the
/// `org/model` layout that mirrors and `hf download --local-dir` produce. Deeper recursion
/// is deliberately avoided — a model directory can hold thousands of files and a runaway
/// walk would make the Models view feel broken.
pub fn scan(roots: &[PathBuf], ftw_dir: &Path) -> Vec<Model> {
    // Keyed by path *and* variant, not by path alone. A repo that ships one file per
    // quantization keeps every build in the snapshot root, so several `Model`s legitimately
    // share a path and differ only in which files they are; keying on the path dropped all
    // but the last, and the one that survived was whichever the directory listing happened
    // to sort last.
    let mut found: BTreeMap<(PathBuf, Option<String>), Model> = BTreeMap::new();
    for root in roots {
        let root = expand_tilde(root);
        if !root.is_dir() {
            continue;
        }
        for entry in read_dir_sorted(&root) {
            if !entry.is_dir() {
                continue;
            }
            // Tried first, and cheap to reject: it only matches a `models--org--name`
            // name, so a plain library directory falls straight through to the rest.
            let cached = inspect_cache_entry(&entry);
            if !cached.is_empty() {
                for m in cached {
                    found.insert(key_of(&m), m);
                }
                continue;
            }
            if let Some(m) = inspect(&entry) {
                found.insert(key_of(&m), m);
                continue;
            }
            // Not a model itself: try one level deeper for the org/model layout.
            for child in read_dir_sorted(&entry) {
                if child.is_dir() {
                    if let Some(m) = inspect(&child) {
                        found.insert(key_of(&m), m);
                    }
                }
            }
        }
    }

    let mut models: Vec<Model> = found.into_values().collect();
    link_conversions(&mut models, ftw_dir);
    models.sort_by_key(|m| m.name.to_lowercase());
    models
}

/// What makes a checkpoint the same checkpoint when two roots both see it: where it is,
/// and which build of that directory it is.
fn key_of(m: &Model) -> (PathBuf, Option<String>) {
    (m.path.clone(), m.variant.clone())
}

/// Resolve a Hugging Face hub cache entry to the checkpoint it currently points at.
///
/// `None` for anything that is not a cache entry, which is what makes this safe to try
/// ahead of the ordinary layouts.
fn inspect_cache_entry(dir: &Path) -> Vec<Model> {
    let Some(repo) = dir.file_name().and_then(|n| n.to_str()).and_then(repo_from_cache_dir) else {
        return Vec::new();
    };
    let Some(snapshot) = cache_snapshot(dir) else { return Vec::new() };

    // Grouped by exactly the rules the Hub tab uses, so a quantization looks the same
    // before and after it is downloaded.
    let layout = crate::variants::analyze(&local_listing(&snapshot));
    let mut out = Vec::new();
    for v in layout.weights() {
        // A quantization in its own directory is a separate checkpoint on disk, and the
        // engine has to be pointed at that directory: a snapshot root holding eleven
        // builds is not something `--model-path` can resolve.
        let path = match &v.subdir {
            Some(sub) => snapshot.join(sub),
            None => snapshot.clone(),
        };
        let Some(mut m) = inspect(&path) else { continue };
        // `inspect` names a directory after itself, which here is a commit sha or a bare
        // quantization label. Neither identifies the model.
        // Tagged whenever the label names a real build, not merely when several are
        // present. Only one quantization may be downloaded today and a second tomorrow,
        // and the two have genuinely different cache costs — `costs.json` is keyed by
        // this name, so letting them share one would price the second against the first.
        let tagged = crate::variants::is_build_label(&v.label);
        m.name = if tagged { format!("{repo}:{}", v.label) } else { repo.clone() };
        m.repo = Some(repo.clone());
        m.variant = tagged.then(|| v.label.clone());
        // Taken from the grouping rather than from the directory, so a quantization whose
        // shards sit beside ten other builds is not reported as the whole shelf.
        if v.bytes > 0 {
            m.size_bytes = v.bytes;
        }
        out.push(m);
    }
    out
}

/// A snapshot's files, shaped like a Hub listing so [`crate::variants::analyze`] can group
/// local and remote repos with one set of rules.
///
/// Two levels deep, which is all the layout ever uses: `UD-IQ3_XXS/shard.gguf`.
fn local_listing(snapshot: &Path) -> Vec<crate::hub::Sibling> {
    let mut out = Vec::new();
    let mut push = |path: &Path, rel: String| {
        // Follows symlinks: in a hub cache the file is a link into `blobs/`.
        let size = std::fs::metadata(path).map(|m| m.len()).ok();
        out.push(crate::hub::Sibling { path: rel, size });
    };
    for entry in read_dir_sorted(snapshot) {
        let Some(name) = entry.file_name().and_then(|n| n.to_str()).map(str::to_string) else {
            continue;
        };
        if entry.is_dir() {
            for child in read_dir_sorted(&entry) {
                if let Some(c) = child.file_name().and_then(|n| n.to_str()) {
                    push(&child, format!("{name}/{c}"));
                }
            }
        } else {
            push(&entry, name);
        }
    }
    out
}

/// Decode a hub cache directory name back into a repo id.
///
/// `huggingface_hub` builds these as `models--` followed by the repo id with `/` replaced
/// by `--`. An organization name cannot contain `--`, so the first separator splits it
/// from the model name; the model name may contain further ones and is taken whole.
fn repo_from_cache_dir(dir_name: &str) -> Option<String> {
    let (org, name) = dir_name.strip_prefix("models--")?.split_once("--")?;
    (!org.is_empty() && !name.is_empty()).then(|| format!("{org}/{name}"))
}

/// The snapshot directory a cache entry resolves to.
///
/// `refs/main` is what a bare repo id resolves to for every other tool, so it is the
/// revision to show. A repo pinned to a tag or a commit has no `main` ref; falling back to
/// the most recently written snapshot lists it rather than hiding it, and an unlisted
/// checkpoint is one nothing can act on.
pub(crate) fn cache_snapshot(repo_dir: &Path) -> Option<PathBuf> {
    let snapshots = repo_dir.join("snapshots");
    if let Ok(sha) = std::fs::read_to_string(repo_dir.join("refs").join("main")) {
        let pinned = snapshots.join(sha.trim());
        if pinned.is_dir() {
            return Some(pinned);
        }
    }
    read_dir_sorted(&snapshots)
        .into_iter()
        .filter(|p| p.is_dir())
        .filter_map(|p| Some((std::fs::metadata(&p).ok()?.modified().ok()?, p)))
        .max_by_key(|(t, _)| *t)
        .map(|(_, p)| p)
}

/// Pair each HF checkpoint with the FTW directory converted from it, by the naming
/// convention [`ftw_output_path`] applies.
fn link_conversions(models: &mut [Model], ftw_dir: &Path) {
    let ftw: Vec<PathBuf> =
        models.iter().filter(|m| m.format == Format::Ftw).map(|m| m.path.clone()).collect();
    for m in models.iter_mut().filter(|m| m.format == Format::Hf) {
        let expected = ftw_output_path(&m.path, m.repo.as_deref(), m.variant.as_deref(), ftw_dir);
        // The sibling `<source>-ftw` is where builds went before `library.ftw_dir` existed.
        // Still accepted as a fallback: an existing build is tens of gigabytes, and a
        // version bump that quietly unlinked it would offer to convert the model again.
        let legacy = legacy_ftw_path(&m.path);
        if let Some(path) = ftw
            .iter()
            .find(|p| **p == expected)
            .or_else(|| ftw.iter().find(|p| Some(*p) == legacy.as_ref()))
        {
            m.converted_to = Some(path.clone());
        }
    }
}

/// Where earlier versions of ft-man wrote an FTW build: beside the source, `<name>-ftw`.
fn legacy_ftw_path(source: &Path) -> Option<PathBuf> {
    let name = source.file_name()?.to_string_lossy().into_owned();
    Some(source.parent()?.join(format!("{name}-ftw")))
}

/// Where ft-man puts the FTW build of a checkpoint: under `library.ftw_dir`, with a
/// `-ftw` suffix.
///
/// Deliberately not beside the source. A hub-cache checkpoint's sibling would be inside
/// `snapshots/`, a tree that belongs to `huggingface_hub` — a directory it did not write
/// is invisible to `hf cache scan` and at risk from `hf cache delete`. An FTW build is
/// also not a Hugging Face repo: it has no repo id and no revision, so the cache has
/// nowhere to file it even in principle.
///
/// The name is org-qualified whenever the origin is known, for the same reason the cache's
/// own names are: two organizations publishing the same model name is common, and a flat
/// name silently merges them into one directory.
pub fn ftw_output_path(
    source: &Path,
    repo: Option<&str>,
    variant: Option<&str>,
    ftw_dir: &Path,
) -> PathBuf {
    let mut stem = match repo {
        Some(r) => r.replace('/', "--"),
        None => source
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "model".into()),
    };
    // Without this, converting two quantizations of one repo would write both builds to
    // the same directory, and the second would silently overwrite the first.
    if let Some(v) = variant {
        stem.push_str("--");
        stem.push_str(v);
    }
    expand_tilde(ftw_dir).join(format!("{stem}-ftw"))
}

/// Identify a single directory, returning `None` when it holds no recognizable model.
pub fn inspect(dir: &Path) -> Option<Model> {
    let name = dir.file_name()?.to_string_lossy().into_owned();
    let modified = std::fs::metadata(dir).and_then(|m| m.modified()).ok();

    if dir.join(crate::ft::proc::FTW_INDEX).is_file() {
        let index = read_json(&dir.join(crate::ft::proc::FTW_INDEX));
        let cfg = read_config(dir);
        return Some(Model {
            name,
            path: dir.to_path_buf(),
            format: Format::Ftw,
            size_bytes: weight_bytes(dir, &["ftw"]),
            arch: cfg.as_ref().and_then(|c| c.first_arch()),
            model_type: cfg.as_ref().and_then(|c| c.model_type.clone()),
            is_moe: cfg.as_ref().is_some_and(|c| c.is_moe()),
            num_experts: cfg.as_ref().and_then(|c| c.num_experts()),
            num_layers: cfg.as_ref().and_then(|c| c.num_layers()),
            quant: index
                .as_ref()
                .and_then(|i| i.get("quant_format"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .or_else(|| cfg.as_ref().and_then(|c| c.quant_method())),
            max_position: cfg.as_ref().and_then(|c| c.max_position()),
            ftw_fingerprint: index
                .as_ref()
                .and_then(|i| i.get("fingerprint"))
                .and_then(|v| v.as_str())
                .map(str::to_string),
            converted_to: None,
            repo: None,
            variant: None,
            modified,
            template_status: crate::templates::status(dir),
            sampling_status: crate::sampling::status(dir),
            sampling_effective: crate::sampling::effective(dir),
            has_inference_config: dir.join("inference/config.json").is_file(),
        });
    }

    // Shards with no index means `ft checkpoint` never reached its finalize step.
    if has_any_extension(dir, &["ftw"]) {
        return Some(Model {
            name,
            path: dir.to_path_buf(),
            format: Format::PartialFtw,
            size_bytes: weight_bytes(dir, &["ftw"]),
            arch: None,
            model_type: None,
            is_moe: false,
            num_experts: None,
            num_layers: None,
            quant: None,
            max_position: None,
            ftw_fingerprint: None,
            converted_to: None,
            repo: None,
            variant: None,
            modified,
            template_status: crate::templates::status(dir),
            sampling_status: crate::sampling::status(dir),
            sampling_effective: crate::sampling::effective(dir),
            has_inference_config: dir.join("inference/config.json").is_file(),
        });
    }

    if dir.join("config.json").is_file() {
        let cfg = read_config(dir)?;
        let has_weights = has_any_extension(dir, &["safetensors", "gguf", "bin"]);
        if !has_weights {
            return None;
        }
        let gguf = has_any_extension(dir, &["gguf"]) && !has_any_extension(dir, &["safetensors"]);
        return Some(Model {
            name,
            path: dir.to_path_buf(),
            format: if gguf { Format::Gguf } else { Format::Hf },
            size_bytes: weight_bytes(dir, &["safetensors", "gguf", "bin"]),
            arch: cfg.first_arch(),
            model_type: cfg.model_type.clone(),
            is_moe: cfg.is_moe(),
            num_experts: cfg.num_experts(),
            num_layers: cfg.num_layers(),
            quant: cfg.quant_method(),
            max_position: cfg.max_position(),
            ftw_fingerprint: None,
            converted_to: None,
            repo: None,
            variant: None,
            modified,
            template_status: crate::templates::status(dir),
            sampling_status: crate::sampling::status(dir),
            sampling_effective: crate::sampling::effective(dir),
            has_inference_config: dir.join("inference/config.json").is_file(),
        });
    }

    // A bare GGUF drop with no config.json still serves (Gemma-4 reads its metadata
    // from the file), so do not require one.
    if has_any_extension(dir, &["gguf"]) {
        return Some(Model {
            name,
            path: dir.to_path_buf(),
            format: Format::Gguf,
            size_bytes: weight_bytes(dir, &["gguf"]),
            arch: None,
            model_type: None,
            is_moe: false,
            num_experts: None,
            num_layers: None,
            quant: None,
            max_position: None,
            ftw_fingerprint: None,
            converted_to: None,
            repo: None,
            variant: None,
            modified,
            template_status: crate::templates::status(dir),
            sampling_status: crate::sampling::status(dir),
            sampling_effective: crate::sampling::effective(dir),
            has_inference_config: dir.join("inference/config.json").is_file(),
        });
    }

    None
}

// ---------------------------------------------------------------- config.json

/// The subset of a Hugging Face `config.json` worth reading. Multimodal checkpoints nest
/// the language model's fields under `text_config`, so every accessor checks both.
#[derive(Debug, Clone, Default, Deserialize)]
struct HfConfig {
    #[serde(default)]
    architectures: Option<Vec<String>>,
    #[serde(default)]
    model_type: Option<String>,
    #[serde(default)]
    num_hidden_layers: Option<u64>,
    #[serde(default)]
    max_position_embeddings: Option<u64>,
    #[serde(default)]
    num_experts: Option<u64>,
    #[serde(default)]
    num_local_experts: Option<u64>,
    #[serde(default)]
    n_routed_experts: Option<u64>,
    #[serde(default)]
    quantization_config: Option<QuantConfig>,
    #[serde(default)]
    text_config: Option<Box<HfConfig>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct QuantConfig {
    #[serde(default)]
    quant_method: Option<String>,
    #[serde(default)]
    fmt: Option<String>,
}

impl HfConfig {
    fn text(&self) -> &HfConfig {
        self.text_config.as_deref().unwrap_or(self)
    }

    fn first_arch(&self) -> Option<String> {
        self.architectures.as_ref().or(self.text().architectures.as_ref())?.first().cloned()
    }

    fn num_experts(&self) -> Option<u64> {
        let pick = |c: &HfConfig| c.num_experts.or(c.num_local_experts).or(c.n_routed_experts);
        pick(self).or_else(|| pick(self.text()))
    }

    fn is_moe(&self) -> bool {
        self.num_experts().is_some_and(|n| n > 0)
            || self.first_arch().map(|a| a.to_lowercase().contains("moe")).unwrap_or(false)
    }

    fn num_layers(&self) -> Option<u64> {
        self.num_hidden_layers.or(self.text().num_hidden_layers)
    }

    fn max_position(&self) -> Option<u64> {
        self.max_position_embeddings.or(self.text().max_position_embeddings)
    }

    fn quant_method(&self) -> Option<String> {
        let q = self.quantization_config.as_ref().or(self.text().quantization_config.as_ref())?;
        q.quant_method.clone().or_else(|| q.fmt.clone())
    }
}

fn read_config(dir: &Path) -> Option<HfConfig> {
    // DeepSeek-V4 keeps the authoritative args in inference/config.json; the top-level
    // one is still the HF-shaped file, so prefer it and fall back.
    let raw = std::fs::read_to_string(dir.join("config.json"))
        .or_else(|_| std::fs::read_to_string(dir.join("inference/config.json")))
        .ok()?;
    serde_json::from_str(&raw).ok()
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

// ---------------------------------------------------------------- fs helpers

fn read_dir_sorted(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    out.sort();
    out
}

fn has_any_extension(dir: &Path, exts: &[&str]) -> bool {
    let Ok(rd) = std::fs::read_dir(dir) else { return false };
    rd.filter_map(|e| e.ok())
        .any(|e| e.path().extension().and_then(|x| x.to_str()).is_some_and(|x| exts.contains(&x)))
}

/// Sum the weight files directly inside `dir`. Checkpoints are flat, so this stays a
/// single readdir rather than a recursive walk.
/// Total bytes of the weight files directly in a directory.
///
/// Resolves symlinks, which is the whole point on a hub cache snapshot: every weight file
/// there is a link into `blobs/`, and `DirEntry::metadata` does not traverse links, so it
/// would report an 80 GiB model as a few hundred bytes. [`dir_size`] deliberately does the
/// opposite — deleting a snapshot frees only the links, not the blobs behind them.
fn weight_bytes(dir: &Path, exts: &[&str]) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    rd.filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()).is_some_and(|x| exts.contains(&x)))
        .filter_map(|p| std::fs::metadata(&p).ok())
        .map(|m| m.len())
        .sum()
}

/// Expand a leading `~` so config files can use it.
pub fn expand_tilde(path: &Path) -> PathBuf {
    let Ok(rest) = path.strip_prefix("~") else { return path.to_path_buf() };
    match dirs::home_dir() {
        Some(home) => home.join(rest),
        None => path.to_path_buf(),
    }
}

/// Recursively total a directory, used before deleting one so the confirmation can say
/// how much space it frees.
pub fn dir_size(dir: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    rd.filter_map(|e| e.ok())
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(t) if t.is_file() => e.metadata().map(|m| m.len()).unwrap_or(0),
            _ => 0,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A repo that keeps one file per quantization in the snapshot root produces several
    /// `Model`s with the same `path`. Keyed on the path alone, all but one were dropped —
    /// and which one survived depended on directory order.
    #[test]
    fn several_builds_of_one_directory_all_survive_the_dedupe() {
        let dir = tmpdir("dedupe");
        let snapshot =
            dir.join("models--unsloth--Qwen3.8-Flash-Next-GGUF").join("snapshots").join("deadbeef");
        std::fs::create_dir_all(&snapshot).unwrap();
        std::fs::write(snapshot.join("config.json"), r#"{"model_type":"qwen3"}"#).unwrap();
        for name in ["Qwen3.8-UD-IQ3_XXS.gguf", "Qwen3.8-Q8_0.gguf"] {
            std::fs::write(snapshot.join(name), vec![0u8; 64]).unwrap();
        }

        let found = scan(std::slice::from_ref(&dir), &dir.join("ftw"));
        let mut variants: Vec<String> = found.iter().filter_map(|m| m.variant.clone()).collect();
        variants.sort();
        assert_eq!(
            variants,
            vec!["Q8_0".to_string(), "UD-IQ3_XXS".to_string()],
            "both builds of one snapshot directory must be listed: {found:#?}"
        );
        // And they really do share a path, which is what the old key collapsed.
        assert_eq!(found[0].path, found[1].path);
        assert_ne!(found[0].served_name(), found[1].served_name());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Builds written before `library.ftw_dir` existed sit beside their source. Those are
    /// tens of gigabytes each, and a version bump that unlinked them would offer to
    /// convert a checkpoint that already had a conversion.
    #[test]
    fn an_ftw_build_in_the_old_sibling_location_stays_linked() {
        let dir = tmpdir("legacy-ftw");
        let source = dir.join("Qwen3.6-35B-A3B");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("config.json"), r#"{"model_type":"qwen3"}"#).unwrap();
        std::fs::write(source.join("model-00001.safetensors"), vec![0u8; 64]).unwrap();

        let sibling = dir.join("Qwen3.6-35B-A3B-ftw");
        std::fs::create_dir_all(&sibling).unwrap();
        std::fs::write(sibling.join(crate::ft::proc::FTW_INDEX), "{}").unwrap();
        std::fs::write(sibling.join("weights.ftw"), vec![0u8; 32]).unwrap();

        // `ftw_dir` points somewhere else entirely, which is the configuration that used
        // to break the link.
        let found = scan(std::slice::from_ref(&dir), &dir.join("elsewhere"));
        let hf = found.iter().find(|m| m.format == Format::Hf).expect("the source is listed");
        assert_eq!(hf.converted_to.as_deref(), Some(sibling.as_path()));
        std::fs::remove_dir_all(&dir).ok();
    }

    fn tmpdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ft-man-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn an_hf_moe_checkpoint_is_recognized() {
        let dir = tmpdir("hf");
        let model = dir.join("Qwen3.6-35B-A3B");
        std::fs::create_dir_all(&model).unwrap();
        std::fs::write(
            model.join("config.json"),
            r#"{"architectures":["Qwen3MoeForCausalLM"],"model_type":"qwen3_moe",
                "num_hidden_layers":48,"max_position_embeddings":262144,"num_experts":128,
                "quantization_config":{"quant_method":"nvfp4"}}"#,
        )
        .unwrap();
        std::fs::write(model.join("model-00001.safetensors"), vec![0u8; 4096]).unwrap();

        let m = inspect(&model).expect("should be recognized");
        assert_eq!(m.format, Format::Hf);
        assert!(m.is_moe);
        assert_eq!(m.num_experts, Some(128));
        assert_eq!(m.quant.as_deref(), Some("nvfp4"));
        assert_eq!(m.max_position, Some(262144));
        assert_eq!(m.size_bytes, 4096);
        assert!(m.convertible());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_ftw_directory_is_recognized_and_is_not_convertible() {
        let dir = tmpdir("ftw");
        let model = dir.join("Qwen-ftw");
        std::fs::create_dir_all(&model).unwrap();
        std::fs::write(
            model.join(crate::ft::proc::FTW_INDEX),
            r#"{"quant_format":"nvfp4","fingerprint":"abc123","shards":[],"total_bytes":0}"#,
        )
        .unwrap();
        std::fs::write(model.join("freetoken-00000.ftw"), vec![0u8; 2048]).unwrap();

        let m = inspect(&model).expect("should be recognized");
        assert_eq!(m.format, Format::Ftw);
        assert_eq!(m.quant.as_deref(), Some("nvfp4"));
        assert_eq!(m.ftw_fingerprint.as_deref(), Some("abc123"));
        assert_eq!(m.size_bytes, 2048);
        assert!(!m.convertible());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_directory_without_weights_is_skipped() {
        let dir = tmpdir("bare");
        let model = dir.join("just-a-config");
        std::fs::create_dir_all(&model).unwrap();
        std::fs::write(model.join("config.json"), r#"{"model_type":"llama"}"#).unwrap();
        assert!(inspect(&model).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn multimodal_configs_read_through_text_config() {
        let dir = tmpdir("mm");
        let model = dir.join("mm-model");
        std::fs::create_dir_all(&model).unwrap();
        std::fs::write(
            model.join("config.json"),
            r#"{"model_type":"gemma4","text_config":{"architectures":["Gemma4ForCausalLM"],
                "num_hidden_layers":34,"max_position_embeddings":131072,"num_experts":64}}"#,
        )
        .unwrap();
        std::fs::write(model.join("model.safetensors"), vec![0u8; 16]).unwrap();

        let m = inspect(&model).unwrap();
        assert_eq!(m.arch.as_deref(), Some("Gemma4ForCausalLM"));
        assert_eq!(m.num_layers, Some(34));
        assert!(m.is_moe);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn scan_pairs_a_checkpoint_with_its_ftw_build() {
        let dir = tmpdir("pair");
        let src = dir.join("Model");
        let ftw = dir.join("Model-ftw");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&ftw).unwrap();
        std::fs::write(src.join("config.json"), r#"{"model_type":"llama"}"#).unwrap();
        std::fs::write(src.join("model.safetensors"), b"x").unwrap();
        std::fs::write(ftw.join(crate::ft::proc::FTW_INDEX), r#"{"quant_format":"bf16"}"#).unwrap();

        let models = scan(std::slice::from_ref(&dir), &dir);
        let hf = models.iter().find(|m| m.format == Format::Hf).unwrap();
        assert_eq!(hf.converted_to.as_deref(), Some(ftw.as_path()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_conversion_that_died_before_its_index_is_listed_as_partial() {
        let dir = tmpdir("partial");
        let out = dir.join("Model-ftw");
        std::fs::create_dir_all(&out).unwrap();
        // Shards written, but `ft checkpoint` never reached its finalize step.
        std::fs::write(out.join("freetoken-00000.ftw"), vec![0u8; 8192]).unwrap();

        let m = inspect(&out).expect("leftovers must be visible, or nothing can delete them");
        assert_eq!(m.format, Format::PartialFtw);
        assert!(m.is_partial());
        assert!(!m.convertible());
        assert_eq!(m.size_bytes, 8192);
        assert!(m.summary().contains("safe to delete"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_partial_is_not_mistaken_for_a_models_completed_conversion() {
        let dir = tmpdir("partiallink");
        let src = dir.join("Model");
        let out = dir.join("Model-ftw");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(src.join("config.json"), r#"{"model_type":"llama"}"#).unwrap();
        std::fs::write(src.join("model.safetensors"), b"x").unwrap();
        std::fs::write(out.join("freetoken-00000.ftw"), b"x").unwrap();

        let models = scan(std::slice::from_ref(&dir), &dir);
        let hf = models.iter().find(|m| m.format == Format::Hf).unwrap();
        assert_eq!(hf.converted_to, None, "a half-written build is not a conversion");
        assert!(models.iter().any(|m| m.is_partial()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_finished_conversion_still_wins_over_the_partial_check() {
        let dir = tmpdir("complete");
        let out = dir.join("Model-ftw");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("freetoken-00000.ftw"), b"x").unwrap();
        std::fs::write(out.join(crate::ft::proc::FTW_INDEX), r#"{"quant_format":"bf16"}"#).unwrap();
        assert_eq!(inspect(&out).unwrap().format, Format::Ftw);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ftw_output_lands_in_the_ftw_dir_not_beside_the_source() {
        let ftw_dir = Path::new("/workspace/models");

        // A plain directory keeps its own name.
        assert_eq!(
            ftw_output_path(Path::new("/models/Qwen3.6-35B-A3B"), None, None, ftw_dir),
            PathBuf::from("/workspace/models/Qwen3.6-35B-A3B-ftw")
        );

        // A cache-resident checkpoint is org-qualified, and — the point of the change —
        // the build does NOT land beside the source, which would be inside `snapshots/`.
        let snapshot = Path::new("/hf/hub/models--unsloth--Qwen3.8/snapshots/abc123");
        let out = ftw_output_path(snapshot, Some("unsloth/Qwen3.8"), None, ftw_dir);
        assert_eq!(out, PathBuf::from("/workspace/models/unsloth--Qwen3.8-ftw"));
        assert!(!out.starts_with("/hf/hub"), "an FTW build must never be written into the cache");

        // Two orgs publishing the same model name must not collide, which is exactly what
        // the old repo-basename scheme did.
        let a = ftw_output_path(snapshot, Some("unsloth/Qwen3.8"), None, ftw_dir);
        let b = ftw_output_path(snapshot, Some("bartowski/Qwen3.8"), None, ftw_dir);
        assert_ne!(a, b);
    }

    #[test]
    fn a_cache_directory_name_decodes_to_its_repo_id() {
        assert_eq!(
            repo_from_cache_dir("models--unsloth--Qwen3.8-Flash-Next-GGUF").as_deref(),
            Some("unsloth/Qwen3.8-Flash-Next-GGUF")
        );
        // A model name may itself contain the separator; only the first one splits.
        assert_eq!(repo_from_cache_dir("models--org--we--ird").as_deref(), Some("org/we--ird"));
        // Not cache entries.
        assert_eq!(repo_from_cache_dir("Qwen3.6-35B-A3B"), None);
        assert_eq!(repo_from_cache_dir("datasets--org--name"), None);
        assert_eq!(repo_from_cache_dir("models--org"), None);
    }

    /// The case that motivated reading the cache at all: a checkpoint downloaded by `hf`,
    /// `from_pretrained`, or another engine on the same machine must show up in the
    /// library, under its repo id rather than a commit sha, at its real size.
    #[test]
    fn a_hub_cache_entry_is_found_named_and_sized() {
        let dir = tmpdir("hubcache");
        let repo = dir.join("models--acme--Tiny-Model");
        let blobs = repo.join("blobs");
        let snap = repo.join("snapshots/abc123def456");
        std::fs::create_dir_all(&blobs).unwrap();
        std::fs::create_dir_all(&snap).unwrap();
        std::fs::create_dir_all(repo.join("refs")).unwrap();
        std::fs::write(repo.join("refs/main"), "abc123def456\n").unwrap();

        std::fs::write(blobs.join("cfg"), r#"{"model_type":"llama"}"#).unwrap();
        std::fs::write(blobs.join("weights"), vec![0u8; 4096]).unwrap();
        // The cache stores content in `blobs/` and links to it per revision. Reading the
        // link rather than its target is what once reported an 80 GiB model as 200 bytes.
        std::os::unix::fs::symlink("../../blobs/cfg", snap.join("config.json")).unwrap();
        std::os::unix::fs::symlink("../../blobs/weights", snap.join("model.safetensors")).unwrap();

        let models = scan(std::slice::from_ref(&dir), Path::new("/workspace/models"));
        assert_eq!(models.len(), 1, "the cache entry must be found: {models:?}");
        let m = &models[0];
        assert_eq!(m.name, "acme/Tiny-Model", "named by repo id, not by commit sha");
        assert_eq!(m.repo.as_deref(), Some("acme/Tiny-Model"));
        assert_eq!(m.format, Format::Hf);
        assert_eq!(m.size_bytes, 4096, "size must resolve through the blob symlink");
        assert_eq!(m.path, snap, "the servable path is the snapshot directory");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A repo pinned to a tag or a commit has no `refs/main`. Listing it anyway beats
    /// hiding a checkpoint that is sitting on disk.
    #[test]
    fn a_cache_entry_without_a_main_ref_falls_back_to_a_snapshot() {
        let dir = tmpdir("hubcache-noref");
        let repo = dir.join("models--acme--Pinned");
        let snap = repo.join("snapshots/deadbeef");
        std::fs::create_dir_all(&snap).unwrap();
        std::fs::write(snap.join("config.json"), r#"{"model_type":"llama"}"#).unwrap();
        std::fs::write(snap.join("model.safetensors"), vec![0u8; 16]).unwrap();

        let models = scan(std::slice::from_ref(&dir), Path::new("/workspace/models"));
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].name, "acme/Pinned");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The bug this fixes, exactly as it appeared: a downloaded quantization lives in a
    /// subdirectory of the snapshot, the snapshot root holds only a projector, and the
    /// library reported an 82 GB model as 862 MiB.
    #[test]
    fn a_snapshot_of_quantizations_lists_each_one_at_its_real_size() {
        let dir = tmpdir("variants-cache");
        let repo = dir.join("models--unsloth--Model-GGUF");
        let snap = repo.join("snapshots/abc123");
        std::fs::create_dir_all(snap.join("UD-IQ3_XXS")).unwrap();
        std::fs::create_dir_all(snap.join("Q8_0")).unwrap();
        std::fs::create_dir_all(repo.join("refs")).unwrap();
        std::fs::write(repo.join("refs/main"), "abc123").unwrap();

        // The only weight file at the snapshot root, and not the model.
        std::fs::write(snap.join("mmproj-F16.gguf"), vec![0u8; 800]).unwrap();
        for i in 1..=3 {
            std::fs::write(
                snap.join(format!("UD-IQ3_XXS/Model-UD-IQ3_XXS-0000{i}-of-00003.gguf")),
                vec![0u8; 10_000],
            )
            .unwrap();
        }
        std::fs::write(snap.join("Q8_0/Model-Q8_0.gguf"), vec![0u8; 50_000]).unwrap();

        let mut models = scan(std::slice::from_ref(&dir), Path::new("/workspace/models"));
        models.sort_by_key(|m| m.size_bytes);
        let names: Vec<&str> = models.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["unsloth/Model-GGUF:UD-IQ3_XXS", "unsloth/Model-GGUF:Q8_0"],
            "each quantization is its own entry, named for the model and the build"
        );

        let small = &models[0];
        assert_eq!(small.size_bytes, 30_000, "the shards of that build, not the projector");
        assert_eq!(
            small.path,
            snap.join("UD-IQ3_XXS"),
            "--model-path must reach the quantization, not a root holding two of them"
        );
        assert_eq!(small.variant.as_deref(), Some("UD-IQ3_XXS"));
        assert_eq!(small.repo.as_deref(), Some("unsloth/Model-GGUF"));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// FreeToken defaults the served name to `basename(model_path)`, which for these paths
    /// is a commit sha or a bare quantization label. ft-man must never let it.
    #[test]
    fn a_served_name_identifies_the_model_not_the_directory() {
        let mut m = inspect(Path::new("/nonexistent")).unwrap_or_else(|| Model {
            name: "abc123def456".into(),
            repo: Some("unsloth/Model-GGUF".into()),
            variant: Some("UD-IQ3_XXS".into()),
            path: PathBuf::from("/hf/hub/models--unsloth--Model-GGUF/snapshots/abc123/UD-IQ3_XXS"),
            format: Format::Gguf,
            size_bytes: 0,
            arch: None,
            model_type: None,
            is_moe: false,
            num_experts: None,
            num_layers: None,
            quant: None,
            max_position: None,
            ftw_fingerprint: None,
            converted_to: None,
            modified: None,
            template_status: Default::default(),
            sampling_status: Default::default(),
            sampling_effective: None,
            has_inference_config: false,
        });
        assert_eq!(m.served_name(), "unsloth/Model-GGUF:UD-IQ3_XXS");

        // A repo with one build needs no tag.
        m.variant = None;
        assert_eq!(m.served_name(), "unsloth/Model-GGUF");

        // A plain directory falls back to its own name, which is meaningful there.
        m.repo = None;
        m.name = "Qwen3.6-35B-A3B".into();
        assert_eq!(m.served_name(), "Qwen3.6-35B-A3B");
    }

    /// Two quantizations of one repo must not converge on one FTW directory.
    #[test]
    fn ftw_builds_of_two_quantizations_do_not_collide() {
        let ftw_dir = Path::new("/workspace/models");
        let src = Path::new("/hf/hub/models--unsloth--M/snapshots/abc/UD-IQ3_XXS");
        let a = ftw_output_path(src, Some("unsloth/M"), Some("UD-IQ3_XXS"), ftw_dir);
        let b = ftw_output_path(src, Some("unsloth/M"), Some("Q8_0"), ftw_dir);
        assert_ne!(a, b);
        assert_eq!(a, PathBuf::from("/workspace/models/unsloth--M--UD-IQ3_XXS-ftw"));
    }
}
