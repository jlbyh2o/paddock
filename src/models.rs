//! The local model library: what is on disk, what format it is in, and how big it is.
//!
//! FreeToken serves two shapes of checkpoint — a Hugging Face directory of safetensors,
//! and its own FTW fast-load directory — and the whole convert workflow is about turning
//! the first into the second. So the scanner's job is to recognize both, pair them up
//! where a conversion already exists, and surface enough metadata (architecture, MoE-ness,
//! quantization, context length) to decide what to do next without opening a JSON file by
//! hand.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Format {
    /// A Hugging Face directory: `config.json` plus safetensors shards.
    Hf,
    /// A converted FreeToken Weight directory.
    Ftw,
    /// A GGUF file or a directory containing one (Gemma-4 loads these natively).
    Gguf,
}

impl Format {
    pub fn label(self) -> &'static str {
        match self {
            Format::Hf => "HF",
            Format::Ftw => "FTW",
            Format::Gguf => "GGUF",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Model {
    pub name: String,
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
    pub modified: Option<std::time::SystemTime>,
}

impl Model {
    /// Short one-line description used in list rows.
    pub fn summary(&self) -> String {
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

    /// Whether converting this checkpoint to FTW is a sensible next action.
    pub fn convertible(&self) -> bool {
        self.format == Format::Hf
    }
}

/// Scan every configured root and return the library, sorted by name.
///
/// Roots are searched at depth 1 and 2: depth 1 catches `~/models/Qwen3.6-35B-A3B`, and
/// depth 2 catches the `org/model` layout that mirrors and `hf download --local-dir`
/// produce. Deeper recursion is deliberately avoided — a model directory can hold
/// thousands of files and a runaway walk would make the Models view feel broken.
pub fn scan(roots: &[PathBuf]) -> Vec<Model> {
    let mut found: BTreeMap<PathBuf, Model> = BTreeMap::new();
    for root in roots {
        let root = expand_tilde(root);
        if !root.is_dir() {
            continue;
        }
        for entry in read_dir_sorted(&root) {
            if !entry.is_dir() {
                continue;
            }
            if let Some(m) = inspect(&entry) {
                found.insert(m.path.clone(), m);
                continue;
            }
            // Not a model itself: try one level deeper for the org/model layout.
            for child in read_dir_sorted(&entry) {
                if child.is_dir() {
                    if let Some(m) = inspect(&child) {
                        found.insert(m.path.clone(), m);
                    }
                }
            }
        }
    }

    let mut models: Vec<Model> = found.into_values().collect();
    link_conversions(&mut models);
    models.sort_by_key(|m| m.name.to_lowercase());
    models
}

/// Pair each HF checkpoint with an FTW directory converted from it. Matching is by the
/// naming convention ft-man itself uses (`<name>-ftw`) and by FTW indexes that record
/// their source path.
fn link_conversions(models: &mut [Model]) {
    let ftw: Vec<(PathBuf, Option<String>)> = models
        .iter()
        .filter(|m| m.format == Format::Ftw)
        .map(|m| (m.path.clone(), m.ftw_fingerprint.clone()))
        .collect();
    for m in models.iter_mut().filter(|m| m.format == Format::Hf) {
        let expected = ftw_output_path(&m.path);
        if let Some((path, _)) = ftw.iter().find(|(p, _)| *p == expected) {
            m.converted_to = Some(path.clone());
        }
    }
}

/// Where ft-man puts the FTW build of a checkpoint: a sibling directory with a `-ftw`
/// suffix, so the pair stays together and the origin stays obvious.
pub fn ftw_output_path(source: &Path) -> PathBuf {
    let name = source
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "model".into());
    let parent = source.parent().unwrap_or(Path::new("."));
    parent.join(format!("{name}-ftw"))
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
            modified,
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
            modified,
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
            modified,
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
fn weight_bytes(dir: &Path, exts: &[&str]) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    rd.filter_map(|e| e.ok())
        .filter(|e| {
            e.path().extension().and_then(|x| x.to_str()).is_some_and(|x| exts.contains(&x))
        })
        .filter_map(|e| e.metadata().ok())
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

        let models = scan(std::slice::from_ref(&dir));
        let hf = models.iter().find(|m| m.format == Format::Hf).unwrap();
        assert_eq!(hf.converted_to.as_deref(), Some(ftw.as_path()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ftw_output_is_a_suffixed_sibling() {
        assert_eq!(
            ftw_output_path(Path::new("/models/Qwen3.6-35B-A3B")),
            PathBuf::from("/models/Qwen3.6-35B-A3B-ftw")
        );
    }
}
