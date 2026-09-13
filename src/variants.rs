//! Grouping a repo's files into the variants a user actually chooses between.
//!
//! A quantized GGUF repo is not a checkpoint, it is a shelf of them. `unsloth/`
//! Qwen3.8-Flash-Next-GGUF holds twelve builds of one model — `BF16`, `Q8_0`, `UD-IQ1_S`
//! through `UD-Q6_K_XL` — plus two multimodal projectors and a set of draft models, in
//! sixty files. Presenting that as sixty checkboxes asks the reader to know which shards
//! belong together, which projector matches, and which directory is a draft model rather
//! than the thing they wanted. Every other tool asks one question instead: which
//! quantization?
//!
//! So the files are grouped here into [`Variant`]s, and the Hub tab picks one. Two layouts
//! cover essentially every repo in the wild:
//!
//! * **A directory per quantization** — `UD-IQ3_XXS/Model-UD-IQ3_XXS-00001-of-00003.gguf`.
//!   The directory name is the label, and it is also what `--model-path` must point at,
//!   because a snapshot's root holds every other quantization too.
//! * **A file per quantization** — `Model-Q4_K_M.gguf` beside `Model-Q8_0.gguf` in the
//!   root. The label comes out of the filename.
//!
//! A plain safetensors checkpoint has exactly one variant, which keeps the rest of the UI
//! from needing a special case for it.

use crate::hub::Sibling;
use serde::Serialize;

/// What a group of files is for. Only [`Role::Weights`] is servable on its own; the others
/// are add-ons that pair with a chosen quantization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// A servable set of weights.
    Weights,
    /// A multimodal projector (`mmproj-*`). Without it a vision model still loads and
    /// still answers — it just silently cannot see, which is why it is selected by
    /// default rather than offered as an extra.
    Projector,
    /// A draft or MTP model for speculative decoding. Off by default: it is an opt-in
    /// feature that needs its own serve flags, and downloading one nobody asked for can
    /// mean tens of gigabytes.
    Draft,
}

#[derive(Debug, Clone, Serialize)]
pub struct Variant {
    /// What the user picks from the list: `UD-IQ3_XXS`, `Q8_0`, `safetensors`.
    pub label: String,
    pub role: Role,
    /// Subdirectory holding the weights, relative to the repo root. `None` means the root.
    ///
    /// This is the value that has to reach `--model-path`: pointing an engine at the root
    /// of a multi-quantization snapshot asks it to choose between twelve builds, and it
    /// cannot.
    pub subdir: Option<String>,
    pub files: Vec<String>,
    pub bytes: u64,
}

impl Variant {
    /// Shards, for a list row that says why one variant is six files and another is three.
    pub fn file_count(&self) -> usize {
        self.files.len()
    }
}

/// A repo's files, grouped.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Layout {
    pub variants: Vec<Variant>,
    /// Files every variant needs regardless of which is chosen: `config.json`, the
    /// tokenizer, a chat template. Small, and useless to make anyone tick individually.
    pub shared: Vec<String>,
}

impl Layout {
    /// The servable quantizations, smallest first.
    pub fn weights(&self) -> impl Iterator<Item = &Variant> {
        self.variants.iter().filter(|v| v.role == Role::Weights)
    }

    /// True when there is a real choice to make. A single-variant repo should not make the
    /// reader pick from a list of one.
    pub fn is_multi(&self) -> bool {
        self.weights().count() > 1
    }

    pub fn get(&self, label: &str) -> Option<&Variant> {
        self.variants.iter().find(|v| v.label == label)
    }

    /// Every file to download for `label`: the variant itself, the shared metadata, and
    /// any projector, which is useless on its own and small enough not to ask about.
    pub fn files_for(&self, label: &str) -> Vec<String> {
        let mut out = self.shared.clone();
        if let Some(v) = self.get(label) {
            out.extend(v.files.iter().cloned());
            if v.role == Role::Weights {
                for p in self.variants.iter().filter(|p| p.role == Role::Projector) {
                    out.extend(p.files.iter().cloned());
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }
}

/// True when a variant label names a specific build rather than standing in for "the
/// whole repo".
///
/// `safetensors` and `gguf` are the fallbacks [`analyze`] uses when a repo ships one
/// unnamed set of weights; everything else came from a quantization directory or filename
/// and is part of the model's identity.
pub fn is_build_label(label: &str) -> bool {
    !matches!(label, "safetensors" | "gguf")
}

/// Files that describe a model rather than hold its weights. Needed by every variant.
fn is_shared(name: &str) -> bool {
    const EXACT: [&str; 7] = [
        "config.json",
        "generation_config.json",
        "tokenizer.json",
        "tokenizer_config.json",
        "special_tokens_map.json",
        "preprocessor_config.json",
        "chat_template.jinja",
    ];
    EXACT.contains(&name) || name.ends_with(".model") || name.ends_with(".jinja")
}

/// Files no serving engine reads: documentation, git plumbing, importance matrices, and
/// the alternate runtimes some repos ship alongside the weights.
fn is_noise(path: &str, name: &str) -> bool {
    const SKIP_DIRS: [&str; 4] = ["onnx/", "coreml/", "openvino/", "tflite/"];
    name.starts_with('.')
        || name.ends_with(".md")
        || name.starts_with("imatrix")
        || name.starts_with("consolidated")
        || SKIP_DIRS.iter().any(|d| path.starts_with(d))
}

/// Recognize a GGUF quantization token inside a name.
///
/// The grammar in practice: an optional `UD-` (Unsloth Dynamic) prefix, then `IQ`/`Q`
/// plus a digit plus optional `_K`/`_0`/`_1` and a size suffix, or one of the float
/// formats. Matched against whole `-`/`.`-separated segments so a model called `Q3-Coder`
/// is not read as a quantization.
pub fn parse_quant(text: &str) -> Option<String> {
    // Scanned rather than split: a quant token contains `_` and `-` itself, so splitting on
    // those separators would take the token apart before it could be matched.
    let upper = text.to_uppercase();
    let bytes = upper.as_bytes();
    for (i, _) in upper.char_indices() {
        // A token must start at a boundary, so `Qwen3` cannot match `Q`.
        if i > 0 && !matches!(bytes[i - 1], b'-' | b'_' | b'.' | b'/') {
            continue;
        }
        let rest = &upper[i..];
        for float in ["BF16", "F32", "F16"] {
            if rest.starts_with(float) && ends_token(rest, float.len()) {
                return Some(float.to_string());
            }
        }
        if let Some(tok) = match_quant_token(rest) {
            // Carry the `UD-` prefix when the repo used one: it is a real distinction
            // between builds, not decoration.
            let ud = upper[..i].ends_with("UD-");
            return Some(if ud { format!("UD-{tok}") } else { tok });
        }
    }
    None
}

/// `IQ3_XXS`, `Q4_K_M`, `Q8_0`, `Q6_K` — from the start of `rest`.
fn match_quant_token(rest: &str) -> Option<String> {
    let b = rest.as_bytes();
    let mut i = if rest.starts_with("IQ") {
        2
    } else if b.first() == Some(&b'Q') {
        1
    } else {
        return None;
    };
    if !b.get(i).is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }
    i += 1;
    // Optional `_K`, `_0`, `_1`, then an optional size suffix (`_M`, `_S`, `_XL`, `_XXS`).
    for _ in 0..2 {
        if b.get(i) == Some(&b'_') {
            let start = i + 1;
            let mut j = start;
            while b.get(j).is_some_and(|c| c.is_ascii_alphanumeric()) {
                j += 1;
            }
            if j == start || j - start > 3 {
                break;
            }
            i = j;
        }
    }
    ends_token(rest, i).then(|| rest[..i].to_string())
}

/// True when position `i` ends a name segment, so `Q8_0` matches in `M-Q8_0-00001` but
/// `Q4` does not match inside `Q4XYZ`.
fn ends_token(text: &str, i: usize) -> bool {
    match text.as_bytes().get(i) {
        None => true,
        Some(c) => matches!(c, b'-' | b'_' | b'.' | b'/'),
    }
}

/// Group a repo's file listing into variants.
pub fn analyze(siblings: &[Sibling]) -> Layout {
    use std::collections::BTreeMap;

    let mut shared: Vec<String> = Vec::new();
    // Keyed by label, preserving the order a group was first seen.
    let mut groups: BTreeMap<String, (Role, Option<String>, Vec<String>, u64)> = BTreeMap::new();

    for s in siblings.iter().filter(|s| !s.path.ends_with('/')) {
        let path = s.path.as_str();
        let (dir, name) = match path.rsplit_once('/') {
            Some((d, n)) => (Some(d.to_string()), n),
            None => (None, path),
        };
        if is_noise(path, name) {
            continue;
        }
        if is_shared(name) {
            shared.push(path.to_string());
            continue;
        }
        let is_weight = name.ends_with(".gguf") || name.ends_with(".safetensors");
        if !is_weight {
            continue;
        }

        let role = if name.starts_with("mmproj") {
            Role::Projector
        } else if name.starts_with("mtp-")
            || dir.as_deref().is_some_and(|d| {
                let d = d.to_uppercase();
                d == "MTP" || d.contains("DRAFT")
            })
        {
            Role::Draft
        } else {
            Role::Weights
        };

        // A projector is named for its own precision, never the model's, so it must not be
        // grouped with a same-named quantization directory.
        let label = match role {
            Role::Projector => {
                parse_quant(name).map(|q| format!("mmproj {q}")).unwrap_or_else(|| "mmproj".into())
            }
            Role::Draft => dir.clone().unwrap_or_else(|| "draft".into()),
            Role::Weights => match &dir {
                // A directory per quantization: the directory name is the label, and it is
                // also the path the engine must be pointed at.
                Some(d) => d.clone(),
                // A file per quantization, or a plain safetensors checkpoint.
                None => parse_quant(name).unwrap_or_else(|| {
                    if name.ends_with(".safetensors") {
                        "safetensors".into()
                    } else {
                        "gguf".into()
                    }
                }),
            },
        };

        let subdir = match role {
            Role::Weights => dir.clone(),
            _ => None,
        };
        let e = groups.entry(label).or_insert((role, subdir, Vec::new(), 0));
        e.2.push(path.to_string());
        e.3 += s.size.unwrap_or(0);
    }

    let mut variants: Vec<Variant> = groups
        .into_iter()
        .map(|(label, (role, subdir, mut files, bytes))| {
            files.sort();
            Variant { label, role, subdir, files, bytes }
        })
        .collect();

    // Smallest first. Size is the ordering every reader already has in mind when choosing a
    // quantization, and unlike a bits-per-weight table it is exact and never goes stale
    // when a new format appears.
    variants.sort_by(|a, b| role_rank(a.role).cmp(&role_rank(b.role)).then(a.bytes.cmp(&b.bytes)));
    shared.sort();
    shared.dedup();
    Layout { variants, shared }
}

fn role_rank(r: Role) -> u8 {
    match r {
        Role::Weights => 0,
        Role::Projector => 1,
        Role::Draft => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sib(path: &str, size: u64) -> Sibling {
        Sibling { path: path.into(), size: Some(size) }
    }

    #[test]
    fn quant_tokens_are_recognized_and_model_names_are_not() {
        assert_eq!(
            parse_quant("Model-UD-IQ3_XXS-00001-of-00003.gguf").as_deref(),
            Some("UD-IQ3_XXS")
        );
        assert_eq!(parse_quant("Model-Q4_K_M.gguf").as_deref(), Some("Q4_K_M"));
        assert_eq!(parse_quant("Model-Q8_0-00001-of-00006.gguf").as_deref(), Some("Q8_0"));
        assert_eq!(parse_quant("Model-Q6_K.gguf").as_deref(), Some("Q6_K"));
        assert_eq!(parse_quant("mmproj-BF16.gguf").as_deref(), Some("BF16"));
        assert_eq!(parse_quant("Model-F16.gguf").as_deref(), Some("F16"));
        // A model whose *name* contains a quant-shaped token must not be misread.
        assert_eq!(parse_quant("Qwen3.8-Flash-Next.gguf"), None);
        assert_eq!(parse_quant("model.safetensors"), None);
    }

    /// The repo that motivated all of this: twelve quantizations in directories, two
    /// projectors and a drawer of draft models, in sixty files.
    #[test]
    fn a_directory_per_quantization_repo_becomes_a_short_list_of_choices() {
        let mut files = vec![
            sib(".gitattributes", 1),
            sib("README.md", 1),
            sib("imatrix_unsloth.gguf_file", 1),
            sib("mmproj-BF16.gguf", 2_000),
            sib("mmproj-F16.gguf", 1_000),
            sib("MTP/mtp-Model-Q8_0.gguf", 500),
        ];
        for i in 1..=3 {
            files.push(sib(&format!("UD-IQ3_XXS/Model-UD-IQ3_XXS-0000{i}-of-00003.gguf"), 10));
        }
        for i in 1..=6 {
            files.push(sib(&format!("Q8_0/Model-Q8_0-0000{i}-of-00006.gguf"), 100));
        }

        let l = analyze(&files);
        let weights: Vec<&str> = l.weights().map(|v| v.label.as_str()).collect();
        assert_eq!(weights, vec!["UD-IQ3_XXS", "Q8_0"], "smallest first, drafts excluded");

        let small = l.get("UD-IQ3_XXS").unwrap();
        assert_eq!(small.file_count(), 3, "the shards of one quantization travel together");
        assert_eq!(small.bytes, 30);
        assert_eq!(
            small.subdir.as_deref(),
            Some("UD-IQ3_XXS"),
            "--model-path must reach the directory, not the snapshot root"
        );

        // Choosing one quantization brings the projectors and leaves the other eleven
        // builds and the draft models on the shelf.
        let picked = l.files_for("UD-IQ3_XXS");
        assert_eq!(picked.iter().filter(|f| f.starts_with("UD-IQ3_XXS/")).count(), 3);
        assert!(picked.iter().any(|f| f.starts_with("mmproj")), "a vision model must keep sight");
        assert!(!picked.iter().any(|f| f.starts_with("Q8_0/")), "no second quantization");
        assert!(!picked.iter().any(|f| f.starts_with("MTP/")), "a draft model is opt-in");
        assert!(!picked.iter().any(|f| f.ends_with(".md")), "no documentation in a download");
    }

    #[test]
    fn a_file_per_quantization_repo_is_grouped_by_filename() {
        let files = vec![
            sib("config.json", 1),
            sib("tokenizer.json", 2),
            sib("Model-Q4_K_M.gguf", 400),
            sib("Model-Q8_0.gguf", 800),
        ];
        let l = analyze(&files);
        let weights: Vec<&str> = l.weights().map(|v| v.label.as_str()).collect();
        assert_eq!(weights, vec!["Q4_K_M", "Q8_0"]);
        // No subdirectory: the engine is pointed at the snapshot root, which is correct
        // here because the quantizations are distinguished by filename.
        assert_eq!(l.get("Q4_K_M").unwrap().subdir, None);

        let picked = l.files_for("Q4_K_M");
        assert!(picked.contains(&"config.json".to_string()), "shared metadata always comes");
        assert!(picked.contains(&"tokenizer.json".to_string()));
        assert!(!picked.contains(&"Model-Q8_0.gguf".to_string()));
    }

    /// A plain checkpoint has one variant, so the UI never asks a question with one answer.
    #[test]
    fn a_safetensors_checkpoint_is_a_single_variant() {
        let files = vec![
            sib("config.json", 1),
            sib("model-00001-of-00002.safetensors", 50),
            sib("model-00002-of-00002.safetensors", 50),
        ];
        let l = analyze(&files);
        assert!(!l.is_multi(), "one choice is not a choice");
        assert_eq!(l.weights().count(), 1);
        let v = l.weights().next().unwrap();
        assert_eq!(v.label, "safetensors");
        assert_eq!(v.bytes, 100);
        assert_eq!(v.subdir, None);
    }

    /// The same repo as it really is on the Hub, listing and sizes fetched from the API —
    /// sixty files, eleven quantizations, two projectors and a drawer of draft models. Synthetic
    /// fixtures agree with whatever the parser happens to do; this one does not.
    #[test]
    fn the_real_unsloth_repo_groups_into_eleven_choices() {
        const REAL: [(&str, u64); 60] = [
            (".gitattributes", 6607),
            ("BF16/Qwen3.8-Flash-Next-BF16-00001-of-00008.gguf", 10946368),
            ("BF16/Qwen3.8-Flash-Next-BF16-00002-of-00008.gguf", 10447201664),
            ("BF16/Qwen3.8-Flash-Next-BF16-00003-of-00008.gguf", 102400491712),
            ("BF16/Qwen3.8-Flash-Next-BF16-00004-of-00008.gguf", 48394236672),
            ("BF16/Qwen3.8-Flash-Next-BF16-00005-of-00008.gguf", 48367899040),
            ("BF16/Qwen3.8-Flash-Next-BF16-00006-of-00008.gguf", 48615233984),
            ("BF16/Qwen3.8-Flash-Next-BF16-00007-of-00008.gguf", 48367899040),
            ("BF16/Qwen3.8-Flash-Next-BF16-00008-of-00008.gguf", 47426022016),
            ("MTP/README.md", 5906),
            ("MTP/mtp-Qwen3.8-Flash-Next-BF16.gguf", 7770760320),
            ("MTP/mtp-Qwen3.8-Flash-Next-Q4_K_M.gguf", 2786204800),
            ("MTP/mtp-Qwen3.8-Flash-Next-Q8_0.gguf", 4137429120),
            ("MTP/mtp-Qwen3.8-Flash-Next-shared-BF16.gguf", 5227963456),
            ("MTP/mtp-Qwen3.8-Flash-Next-shared-Q4_K_M.gguf", 1907151936),
            ("MTP/mtp-Qwen3.8-Flash-Next-shared-Q8_0.gguf", 2786568256),
            ("Q8_0/Qwen3.8-Flash-Next-Q8_0-00001-of-00006.gguf", 10946624),
            ("Q8_0/Qwen3.8-Flash-Next-Q8_0-00002-of-00006.gguf", 682434912),
            ("Q8_0/Qwen3.8-Flash-Next-Q8_0-00003-of-00006.gguf", 54400261312),
            ("Q8_0/Qwen3.8-Flash-Next-Q8_0-00004-of-00006.gguf", 49446841216),
            ("Q8_0/Qwen3.8-Flash-Next-Q8_0-00005-of-00006.gguf", 49668930400),
            ("Q8_0/Qwen3.8-Flash-Next-Q8_0-00006-of-00006.gguf", 34015618784),
            ("README.md", 59183),
            ("UD-IQ1_M/Qwen3.8-Flash-Next-UD-IQ1_M-00001-of-00003.gguf", 10946624),
            ("UD-IQ1_M/Qwen3.8-Flash-Next-UD-IQ1_M-00002-of-00003.gguf", 49988981792),
            ("UD-IQ1_M/Qwen3.8-Flash-Next-UD-IQ1_M-00003-of-00003.gguf", 24538827360),
            ("UD-IQ1_S/Qwen3.8-Flash-Next-UD-IQ1_S-00001-of-00003.gguf", 10946624),
            ("UD-IQ1_S/Qwen3.8-Flash-Next-UD-IQ1_S-00002-of-00003.gguf", 49990818368),
            ("UD-IQ1_S/Qwen3.8-Flash-Next-UD-IQ1_S-00003-of-00003.gguf", 22544696352),
            ("UD-IQ3_XXS/Qwen3.8-Flash-Next-UD-IQ3_XXS-00001-of-00003.gguf", 10946624),
            ("UD-IQ3_XXS/Qwen3.8-Flash-Next-UD-IQ3_XXS-00002-of-00003.gguf", 49567921344),
            ("UD-IQ3_XXS/Qwen3.8-Flash-Next-UD-IQ3_XXS-00003-of-00003.gguf", 32382955968),
            ("UD-IQ4_XS/Qwen3.8-Flash-Next-UD-IQ4_XS-00001-of-00003.gguf", 10946624),
            ("UD-IQ4_XS/Qwen3.8-Flash-Next-UD-IQ4_XS-00002-of-00003.gguf", 49835229856),
            ("UD-IQ4_XS/Qwen3.8-Flash-Next-UD-IQ4_XS-00003-of-00003.gguf", 43836407744),
            ("UD-Q2_K_XL/Qwen3.8-Flash-Next-UD-Q2_K_XL-00001-of-00003.gguf", 10946624),
            ("UD-Q2_K_XL/Qwen3.8-Flash-Next-UD-Q2_K_XL-00002-of-00003.gguf", 49979779296),
            ("UD-Q2_K_XL/Qwen3.8-Flash-Next-UD-Q2_K_XL-00003-of-00003.gguf", 28878402944),
            ("UD-Q3_K_XL/Qwen3.8-Flash-Next-UD-Q3_K_XL-00001-of-00003.gguf", 10946624),
            ("UD-Q3_K_XL/Qwen3.8-Flash-Next-UD-Q3_K_XL-00002-of-00003.gguf", 49983253824),
            ("UD-Q3_K_XL/Qwen3.8-Flash-Next-UD-Q3_K_XL-00003-of-00003.gguf", 39992153376),
            ("UD-Q4_K_XL/Qwen3.8-Flash-Next-UD-Q4_K_XL-00001-of-00004.gguf", 10946624),
            ("UD-Q4_K_XL/Qwen3.8-Flash-Next-UD-Q4_K_XL-00002-of-00004.gguf", 49859583136),
            ("UD-Q4_K_XL/Qwen3.8-Flash-Next-UD-Q4_K_XL-00003-of-00004.gguf", 49376141504),
            ("UD-Q4_K_XL/Qwen3.8-Flash-Next-UD-Q4_K_XL-00004-of-00004.gguf", 12087983520),
            ("UD-Q5_K_XL/Qwen3.8-Flash-Next-UD-Q5_K_XL-00001-of-00006.gguf", 10946618),
            ("UD-Q5_K_XL/Qwen3.8-Flash-Next-UD-Q5_K_XL-00002-of-00006.gguf", 682434912),
            ("UD-Q5_K_XL/Qwen3.8-Flash-Next-UD-Q5_K_XL-00003-of-00006.gguf", 54400261312),
            ("UD-Q5_K_XL/Qwen3.8-Flash-Next-UD-Q5_K_XL-00004-of-00006.gguf", 49990245824),
            ("UD-Q5_K_XL/Qwen3.8-Flash-Next-UD-Q5_K_XL-00005-of-00006.gguf", 49882416192),
            ("UD-Q5_K_XL/Qwen3.8-Flash-Next-UD-Q5_K_XL-00006-of-00006.gguf", 3320101792),
            ("UD-Q6_K_XL/Qwen3.8-Flash-Next-UD-Q6_K_XL-00001-of-00006.gguf", 10946624),
            ("UD-Q6_K_XL/Qwen3.8-Flash-Next-UD-Q6_K_XL-00002-of-00006.gguf", 682434912),
            ("UD-Q6_K_XL/Qwen3.8-Flash-Next-UD-Q6_K_XL-00003-of-00006.gguf", 54400261312),
            ("UD-Q6_K_XL/Qwen3.8-Flash-Next-UD-Q6_K_XL-00004-of-00006.gguf", 49814108512),
            ("UD-Q6_K_XL/Qwen3.8-Flash-Next-UD-Q6_K_XL-00005-of-00006.gguf", 49419317344),
            ("UD-Q6_K_XL/Qwen3.8-Flash-Next-UD-Q6_K_XL-00006-of-00006.gguf", 14838313984),
            ("imatrix_unsloth.gguf_file", 580038720),
            ("mmproj-BF16.gguf", 907542944),
            ("mmproj-F16.gguf", 904004000),
        ];
        let files: Vec<Sibling> = REAL.iter().map(|(p, s)| sib(p, *s)).collect();
        let l = analyze(&files);

        let weights: Vec<&str> = l.weights().map(|v| v.label.as_str()).collect();
        assert_eq!(
            weights,
            vec![
                "UD-IQ1_S",
                "UD-IQ1_M",
                "UD-Q2_K_XL",
                "UD-IQ3_XXS",
                "UD-Q3_K_XL",
                "UD-IQ4_XS",
                "UD-Q4_K_XL",
                "UD-Q5_K_XL",
                "UD-Q6_K_XL",
                "Q8_0",
                "BF16",
            ],
            "every quantization, smallest first, and nothing else"
        );

        // Picking one gets its shards, the shared metadata and the projectors — not the
        // other eleven builds, and not the draft models.
        let picked = l.files_for("UD-IQ3_XXS");
        assert!(
            picked.iter().all(|f| {
                f.starts_with("UD-IQ3_XXS/") || f.starts_with("mmproj") || !f.contains('/')
            }),
            "{picked:?}"
        );
        assert_eq!(picked.iter().filter(|f| f.starts_with("UD-IQ3_XXS/")).count(), 3);
        assert!(!picked.iter().any(|f| f.starts_with("MTP/")));
        assert!(!picked.iter().any(|f| f.starts_with("BF16/")));

        // The choice that matters most is size, and it has to be right.
        let chosen = l.get("UD-IQ3_XXS").unwrap();
        assert_eq!(chosen.subdir.as_deref(), Some("UD-IQ3_XXS"));
        // ~82 GB, which is what the three shards of this variant really weigh — and what
        // `du` reports for it on a machine that has downloaded it.
        assert!(
            chosen.bytes > 75_000_000_000 && chosen.bytes < 90_000_000_000,
            "UD-IQ3_XXS should be about 82 GB, got {}",
            chosen.bytes
        );
        // BF16 is the biggest by a wide margin, which is the whole reason the list is
        // ordered by size rather than by name.
        assert!(l.get("BF16").unwrap().bytes > chosen.bytes * 4);
        assert!(l.get("UD-IQ1_S").unwrap().bytes < chosen.bytes);
    }
}
