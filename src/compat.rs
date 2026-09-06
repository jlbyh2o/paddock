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

    // ---- the multimodal quantization split ----
    //
    // A `...ForConditionalGeneration` wrapper keeps the language model under
    // `text_config` while `quantization_config` stays at the top level. FreeToken reads
    // the nested config when resolving expert quantization, finds no quantization block,
    // and settles on "none" — after which the converter looks for unquantized expert
    // tensors, does not find the packed ones, and fails with "Missing MoE expert source
    // layers" minutes in. Verified against Qwen3_5MoeForConditionalGeneration.
    let quant_only_at_top = config.get("quantization_config").is_some()
        && text.is_some_and(|t| t.get("quantization_config").is_none());
    if r.is_moe && quant_only_at_top {
        note(
            Level::Blocker,
            format!(
                "quantization_config sits at the top level while the language model is under \
                 text_config, so FreeToken resolves its experts as unquantized{} — conversion \
                 and serving fail with 'Missing MoE expert source layers'",
                r.quant
                    .as_deref()
                    .map(|q| format!(" despite the checkpoint declaring {q}"))
                    .unwrap_or_default()
            ),
        );
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
            "quantization_config": {"quant_method": "compressed-tensors", "format": "nvfp4-pack-quantized"}
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

    /// The exact shape that cost a 23 GiB download and two failed conversions.
    #[test]
    fn the_multimodal_quantization_split_is_caught() {
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
        // The architecture IS registered — the wall is elsewhere, and saying "unsupported
        // architecture" would send someone looking in the wrong place.
        assert!(!r.notes.iter().any(|(_, m)| m.contains("model registry")));
        assert_eq!(r.verdict(), Verdict::Unsupported);
        let msg = &r.notes[0].1;
        assert!(msg.contains("text_config"), "{msg}");
        assert!(msg.contains("Missing MoE expert source layers"), "{msg}");
        // Fields still resolve through text_config.
        assert_eq!(r.num_experts, Some(256));
        assert_eq!(r.num_layers, Some(40));
    }

    #[test]
    fn a_multimodal_model_that_nests_its_quant_config_is_fine() {
        let cfg = json!({
            "architectures": ["Qwen3_5MoeForConditionalGeneration"],
            "quantization_config": {"format": "nvfp4-pack-quantized"},
            "text_config": {
                "num_experts": 256,
                "quantization_config": {"format": "nvfp4-pack-quantized"}
            }
        });
        let r = evaluate(&cfg, 23 << 30, Some(&archs()), roomy());
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
