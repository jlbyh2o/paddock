//! Judging whether FreeToken can run a checkpoint, from its `config.json` alone.
//!
//! A weights download is tens of gigabytes and half an hour; a `config.json` is a single
//! small request. Almost everything that decides compatibility lives in that file plus
//! the repo's file listing, so the question is worth answering before committing to the
//! download rather than after.
//!
//! What this can decide, and what it cannot: the architecture check is definitive,
//! because FreeToken's own registry is the authority and it is queried directly. The
//! quantization check catches one specific, verified failure mode. Everything else is
//! arithmetic against the hardware. A clean report is not a promise that a model will
//! serve — only that none of the known walls are in the way.

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Worth knowing, not a problem.
    Info,
    /// Will work, but constrains how.
    Caution,
    /// Expected to fail.
    Blocker,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// No known obstacle.
    Supported,
    /// Will load, but something about it needs attention.
    Caution,
    /// A known wall.
    Unsupported,
    /// Not enough information — usually the architecture registry is not loaded yet.
    Unknown,
}

impl Verdict {
    pub fn label(&self) -> &'static str {
        match self {
            Verdict::Supported => "supported",
            Verdict::Caution => "supported, with caveats",
            Verdict::Unsupported => "not supported",
            Verdict::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Report {
    pub arch: Option<String>,
    pub model_type: Option<String>,
    pub is_moe: bool,
    pub num_experts: Option<u64>,
    pub num_layers: Option<u64>,
    pub quant: Option<String>,
    pub context: Option<u64>,
    /// `(level, message)`, most severe first.
    pub notes: Vec<(Level, String)>,
}

impl Report {
    pub fn verdict(&self) -> Verdict {
        match self.notes.iter().map(|(l, _)| *l).max() {
            Some(Level::Blocker) => Verdict::Unsupported,
            Some(Level::Caution) => Verdict::Caution,
            Some(Level::Info) | None => {
                if self.arch.is_some() {
                    Verdict::Supported
                } else {
                    Verdict::Unknown
                }
            }
        }
    }

    /// One-line shape summary for a header.
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
        } else if self.arch.is_some() {
            parts.push("dense".into());
        }
        if let Some(q) = &self.quant {
            parts.push(q.to_uppercase());
        }
        if let Some(c) = self.context {
            parts.push(format!("{}k ctx", c / 1024));
        }
        parts.join(" · ")
    }
}

/// The hardware a candidate would have to run on.
#[derive(Debug, Clone, Copy, Default)]
pub struct Hardware {
    pub vram_bytes: u64,
    pub host_ram_bytes: u64,
    pub free_disk_bytes: u64,
}

/// Evaluate a repo from its config and the size of the files that would be downloaded.
///
/// `supported_archs` is FreeToken's own registry; `None` means it could not be consulted,
/// which is reported rather than guessed at.
pub fn evaluate(
    config: &Value,
    download_bytes: u64,
    supported_archs: Option<&[String]>,
    hw: Hardware,
) -> Report {
    let mut r = Report::default();
    let text = config.get("text_config");

    // Read a field from the top level or, for a multimodal wrapper, from text_config.
    let field = |key: &str| -> Option<&Value> {
        config
            .get(key)
            .filter(|v| !v.is_null())
            .or_else(|| text.and_then(|t| t.get(key)).filter(|v| !v.is_null()))
    };

    r.arch = config
        .get("architectures")
        .or_else(|| text.and_then(|t| t.get("architectures")))
        .and_then(|a| a.as_array())
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
        .map(str::to_string);
    r.model_type = field("model_type").and_then(|v| v.as_str()).map(str::to_string);
    r.num_layers = field("num_hidden_layers").and_then(Value::as_u64);
    r.context = field("max_position_embeddings").and_then(Value::as_u64);
    r.num_experts = ["num_experts", "num_local_experts", "n_routed_experts"]
        .iter()
        .find_map(|k| field(k).and_then(Value::as_u64))
        .filter(|n| *n > 0);
    r.is_moe = r.num_experts.is_some()
        || r.arch.as_deref().is_some_and(|a| a.to_lowercase().contains("moe"));

    let quant_block = config
        .get("quantization_config")
        .or_else(|| text.and_then(|t| t.get("quantization_config")));
    r.quant = quant_block.and_then(|q| {
        ["format", "quant_algo", "quant_method"]
            .iter()
            .find_map(|k| q.get(*k).and_then(|v| v.as_str()))
            .map(str::to_string)
    });

    let mut note = |level: Level, msg: String| r.notes.push((level, msg));

    // ---- architecture: FreeToken's registry is the authority ----
    match (&r.arch, supported_archs) {
        (None, _) => note(
            Level::Blocker,
            "config.json declares no architecture; FreeToken cannot dispatch a loader".into(),
        ),
        (Some(arch), Some(known)) if !known.iter().any(|k| k == arch) => {
            note(Level::Blocker, format!("{arch} is not in FreeToken's model registry"))
        }
        (Some(_), None) => note(
            Level::Caution,
            "could not consult FreeToken's model registry, so architecture support is unverified"
                .into(),
        ),
        (Some(_), Some(_)) => {}
    }

    // ---- can FreeToken resolve the routed experts? ----
    //
    // Historically this was a blocker: FreeToken's Qwen3.5-MoE family carried its own
    // expert-quantization detector that never looked at `format`, so an llm-compressor
    // (compressed-tensors) export resolved to "none" and died deep in the bank loader.
    // FreeToken #418/#427/#438 replaced every family-local detector with one QuantConfig
    // layer that reads each module's scheme by name, in either dialect, so that whole
    // class of failure is gone. What remains is the one case a config can still get
    // wrong on its own terms: a modelopt MIXED_PRECISION allow-list that never names the
    // experts.
    if r.is_moe {
        if let Some((level, reason)) = unresolvable_experts(quant_block) {
            note(level, reason);
        }
    }

    // ---- hardware ----
    if download_bytes > 0 {
        if hw.free_disk_bytes > 0 && download_bytes > hw.free_disk_bytes {
            note(
                Level::Blocker,
                format!(
                    "needs {} but only {} is free on the download filesystem",
                    crate::util::bytes(download_bytes),
                    crate::util::bytes(hw.free_disk_bytes)
                ),
            );
        } else if hw.free_disk_bytes > 0 && download_bytes * 2 > hw.free_disk_bytes {
            // FTW conversion writes a second full copy alongside the original.
            note(
                Level::Caution,
                format!(
                    "{} free leaves no room to also convert it to FTW, which writes a second \
                     copy of about {}",
                    crate::util::bytes(hw.free_disk_bytes),
                    crate::util::bytes(download_bytes)
                ),
            );
        }

        if r.is_moe {
            if hw.host_ram_bytes > 0 && download_bytes > hw.host_ram_bytes {
                note(
                    Level::Caution,
                    format!(
                        "offloaded expert banks live in host RAM: {} of weights against {} of \
                         RAM",
                        crate::util::bytes(download_bytes),
                        crate::util::bytes(hw.host_ram_bytes)
                    ),
                );
            }
            if hw.vram_bytes > 0 && download_bytes > hw.vram_bytes {
                note(
                    Level::Info,
                    format!(
                        "larger than {} of VRAM, so it needs an offload MoE backend rather than \
                         fused",
                        crate::util::bytes(hw.vram_bytes)
                    ),
                );
            }
        } else if hw.vram_bytes > 0 && download_bytes > hw.vram_bytes {
            // Dense models resolve to the resident backend, which has nowhere to spill.
            note(
                Level::Blocker,
                format!(
                    "a dense model is served resident, and {} of weights does not fit in {} of \
                     VRAM",
                    crate::util::bytes(download_bytes),
                    crate::util::bytes(hw.vram_bytes)
                ),
            );
        }
    }

    // ---- checkpoint-specific requirements from FreeToken's docs ----
    if r.model_type.as_deref().is_some_and(|m| m.contains("deepseek"))
        || r.arch.as_deref().is_some_and(|a| a.starts_with("DeepseekV4"))
    {
        note(
            Level::Caution,
            "DeepSeek-V4 reads its authoritative args from an inference/config.json subdirectory; \
             make sure the repo ships one"
                .into(),
        );
    }

    // Most severe first: the blocker must be the first thing read.
    r.notes.sort_by_key(|(level, _)| std::cmp::Reverse(*level));
    r
}

/// Whether FreeToken will struggle to resolve a MoE checkpoint's routed-expert
/// quantization.
///
/// Mirrors `QuantConfig`: a dialect is chosen from `quant_method`/`quant_algo`, and each
/// module's scheme is then looked up by name. compressed-tensors and plain modelopt
/// exports both answer for themselves, so the only shape still worth flagging is a
/// modelopt `MIXED_PRECISION` allow-list with no entry covering the experts — the
/// checkpoint's own config saying the experts are unquantized when its tensors are not.
///
/// Returns the severity and the reason, or `None` when the experts resolve or there is
/// not enough evidence to say.
fn unresolvable_experts(quant: Option<&Value>) -> Option<(Level, String)> {
    let quant = quant?;
    let get = |k: &str| quant.get(k).and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
    let algo = {
        let a = get("quant_algo");
        if a.is_empty() {
            get("quant_method")
        } else {
            a
        }
    };

    // Block-FP8 and anything naming fp4 outright are recognized.
    if algo.contains("fp4") || (algo == "fp8" && quant.get("weight_block_size").is_some()) {
        return None;
    }

    // modelopt MIXED_PRECISION: the routed experts carry their own algo in a per-layer map.
    if algo.contains("mixed") {
        let layers = quant.get("quantized_layers").and_then(Value::as_object);
        let experts_covered = layers.is_some_and(|m| {
            m.iter().any(|(name, spec)| {
                (name.ends_with(".mlp.experts") || name.contains(".mlp.experts."))
                    && spec.get("quant_algo").and_then(|v| v.as_str()).is_some_and(|a| {
                        let a = a.to_lowercase();
                        a.contains("fp4") || a.contains("fp8")
                    })
            })
        });
        return (!experts_covered).then(|| {
            (
                Level::Caution,
                "the checkpoint is a MIXED_PRECISION export but its quantized_layers map lists                  no .mlp.experts entry, so FreeToken reads the routed experts as unquantized.                  If the tensors are in fact quantized, the loader rejects the checkpoint for                  disagreeing with its own quant config"
                    .to_string(),
            )
        });
    }

    // compressed-tensors (llm-compressor) exports are read natively: the dialect declares
    // its own tensor names (weight_packed / weight_global_scale) and whether the stored
    // global is the quant-side scale, so no family needs to carry a translation.
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn archs() -> Vec<String> {
        ["Qwen3MoeForCausalLM", "Qwen3_5MoeForConditionalGeneration", "LlamaForCausalLM"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    fn roomy() -> Hardware {
        Hardware { vram_bytes: 16 << 30, host_ram_bytes: 40 << 30, free_disk_bytes: 500 << 30 }
    }

    #[test]
    fn a_plain_supported_moe_is_reported_supported() {
        let cfg = json!({
            "architectures": ["Qwen3MoeForCausalLM"],
            "model_type": "qwen3_moe",
            "num_experts": 128,
            "num_hidden_layers": 48,
            "max_position_embeddings": 262144,
            "quantization_config": {"quant_algo": "NVFP4"}
        });
        let r = evaluate(&cfg, 20 << 30, Some(&archs()), roomy());
        assert_eq!(r.verdict(), Verdict::Supported, "{:?}", r.notes);
        assert!(r.is_moe);
        assert_eq!(r.num_experts, Some(128));
        assert!(r.summary().contains("MoE x128"));
    }

    #[test]
    fn an_unregistered_architecture_is_a_blocker() {
        let cfg = json!({"architectures": ["SomeNewThingForCausalLM"], "model_type": "x"});
        let r = evaluate(&cfg, 0, Some(&archs()), roomy());
        assert_eq!(r.verdict(), Verdict::Unsupported);
        assert!(r.notes[0].1.contains("not in FreeToken's model registry"));
    }

    /// An llm-compressor (compressed-tensors) NVFP4 MoE export. FreeToken used to resolve
    /// its experts as unquantized and die in the bank loader; since the QuantConfig layer
    /// (#418/#427/#438) it reads the dialect natively, so flagging it would now steer
    /// someone away from a checkpoint that serves. This is the Ornith-1.5-35B-A3B-NVFP4
    /// shape that motivated the original check.
    #[test]
    fn an_llm_compressor_moe_export_is_served_natively() {
        let cfg = json!({
            "architectures": ["Qwen3_5MoeForConditionalGeneration"],
            "model_type": "qwen3_5_moe",
            "quantization_config": {
                "quant_method": "compressed-tensors",
                "format": "nvfp4-pack-quantized"
            },
            "text_config": {
                "num_experts": 256,
                "num_hidden_layers": 40,
                "max_position_embeddings": 262144
            }
        });
        let r = evaluate(&cfg, 23 << 30, Some(&archs()), roomy());
        assert_eq!(r.verdict(), Verdict::Supported, "{:?}", r.notes);
        assert!(!r.notes.iter().any(|(_, m)| m.contains("model registry")));
        // Fields still resolve through text_config.
        assert_eq!(r.num_experts, Some(256));
        assert_eq!(r.num_layers, Some(40));
    }

    /// The dialect is read per module, not per family, so a compressed-tensors export is
    /// unflagged on every architecture -- not just the one that once carried a translation.
    #[test]
    fn compressed_tensors_is_not_flagged_on_any_family() {
        let cfg = json!({
            "architectures": ["Glm5NextForConditionalGeneration"],
            "quantization_config": {"quant_method": "compressed-tensors", "format": "nvfp4-pack-quantized"},
            "text_config": {"num_experts": 160}
        });
        let mut known = archs();
        known.push("Glm5NextForConditionalGeneration".into());
        let r = evaluate(&cfg, 20 << 30, Some(&known), roomy());
        assert_eq!(r.verdict(), Verdict::Supported, "{:?}", r.notes);
    }

    /// A modelopt MIXED_PRECISION export is fine when its per-layer map covers the experts.
    #[test]
    fn a_mixed_precision_export_is_judged_by_its_quantized_layers_map() {
        let covered = json!({
            "architectures": ["Qwen3_5MoeForConditionalGeneration"],
            "quantization_config": {
                "quant_algo": "MIXED_PRECISION",
                "quantized_layers": {
                    "model.layers.0.mlp.experts": {"quant_algo": "W4A16_NVFP4"},
                    "lm_head": {"quant_algo": "W4A16_NVFP4"}
                }
            },
            "text_config": {"num_experts": 256}
        });
        let r = evaluate(&covered, 20 << 30, Some(&archs()), roomy());
        assert_eq!(r.verdict(), Verdict::Supported, "{:?}", r.notes);

        // The same export with nothing covering the experts cannot resolve them.
        let bare = json!({
            "architectures": ["Qwen3_5MoeForConditionalGeneration"],
            "quantization_config": {
                "quant_algo": "MIXED_PRECISION",
                "quantized_layers": {"lm_head": {"quant_algo": "W4A16_NVFP4"}}
            },
            "text_config": {"num_experts": 256}
        });
        let r = evaluate(&bare, 20 << 30, Some(&archs()), roomy());
        assert_eq!(r.verdict(), Verdict::Caution, "{:?}", r.notes);
        assert!(r.notes[0].1.contains("quantized_layers"), "{:?}", r.notes);
    }

    /// A plain modelopt NVFP4 export names fp4 outright and needs no map.
    #[test]
    fn a_plain_modelopt_nvfp4_export_is_supported() {
        let cfg = json!({
            "architectures": ["Qwen3_5MoeForConditionalGeneration"],
            "quantization_config": {"quant_algo": "NVFP4"},
            "text_config": {"num_experts": 256}
        });
        let r = evaluate(&cfg, 20 << 30, Some(&archs()), roomy());
        assert_eq!(r.verdict(), Verdict::Supported, "{:?}", r.notes);
    }

    /// An unquantized MoE has no expert quantization to resolve, so nothing to flag.
    #[test]
    fn an_unquantized_moe_is_not_flagged() {
        let cfg = json!({
            "architectures": ["Qwen3MoeForCausalLM"],
            "num_experts": 128
        });
        let r = evaluate(&cfg, 20 << 30, Some(&archs()), roomy());
        assert_eq!(r.verdict(), Verdict::Supported, "{:?}", r.notes);
    }

    #[test]
    fn a_dense_model_larger_than_vram_is_a_blocker_but_an_moe_is_not() {
        let dense = json!({"architectures": ["LlamaForCausalLM"], "num_hidden_layers": 80});
        let r = evaluate(&dense, 40 << 30, Some(&archs()), roomy());
        assert_eq!(r.verdict(), Verdict::Unsupported);
        assert!(r.notes[0].1.contains("served resident"));

        let moe = json!({"architectures": ["Qwen3MoeForCausalLM"], "num_experts": 128});
        let r = evaluate(&moe, 40 << 30, Some(&archs()), roomy());
        // Too big for VRAM is normal for MoE — that is what offload is for.
        assert_ne!(r.verdict(), Verdict::Unsupported);
        assert!(r
            .notes
            .iter()
            .any(|(l, m)| *l == Level::Info && m.contains("offload MoE backend")));
    }

    #[test]
    fn disk_and_ram_limits_are_reported_at_the_right_severity() {
        let cfg = json!({"architectures": ["Qwen3MoeForCausalLM"], "num_experts": 128});
        let tight =
            Hardware { vram_bytes: 16 << 30, host_ram_bytes: 40 << 30, free_disk_bytes: 30 << 30 };
        // Fits on disk, but not twice — conversion would not have room.
        let r = evaluate(&cfg, 20 << 30, Some(&archs()), tight);
        assert_eq!(r.verdict(), Verdict::Caution);
        assert!(r.notes.iter().any(|(_, m)| m.contains("second copy")));

        // Does not fit at all.
        let r = evaluate(&cfg, 60 << 30, Some(&archs()), tight);
        assert_eq!(r.verdict(), Verdict::Unsupported);
        assert!(r.notes.iter().any(|(_, m)| m.contains("only")));

        // Bigger than host RAM: the expert banks have nowhere to live.
        let r = evaluate(&cfg, 50 << 30, Some(&archs()), roomy());
        assert!(r.notes.iter().any(|(_, m)| m.contains("host RAM")));
    }

    #[test]
    fn an_unavailable_registry_is_admitted_not_guessed() {
        let cfg = json!({"architectures": ["Whatever"], "num_experts": 4});
        let r = evaluate(&cfg, 0, None, roomy());
        assert_eq!(r.verdict(), Verdict::Caution);
        assert!(r.notes.iter().any(|(_, m)| m.contains("unverified")));
    }

    #[test]
    fn a_config_with_no_architecture_cannot_be_dispatched() {
        let r = evaluate(&json!({"model_type": "mystery"}), 0, Some(&archs()), roomy());
        assert_eq!(r.verdict(), Verdict::Unsupported);
        assert!(r.notes[0].1.contains("no architecture"));
    }

    #[test]
    fn notes_are_ordered_most_severe_first() {
        let cfg = json!({"architectures": ["Nope"], "num_experts": 8});
        let r = evaluate(
            &cfg,
            60 << 30,
            Some(&archs()),
            Hardware { vram_bytes: 16 << 30, host_ram_bytes: 8 << 30, free_disk_bytes: 10 << 30 },
        );
        let levels: Vec<Level> = r.notes.iter().map(|(l, _)| *l).collect();
        let mut sorted = levels.clone();
        sorted.sort_by(|a, b| b.cmp(a));
        assert_eq!(levels, sorted, "the blocker must be the first thing read");
    }
}
