//! The `ft serve` knob schema and the editable configuration built on top of it.
//!
//! Every flag FreeToken's server accepts is described once, here, with its type, domain,
//! default and help text. The Config view renders straight from this table, validation
//! reads it, and [`ServeConfig::to_args`] turns an edited set back into an argv. Mutual
//! exclusions (`--num-pages` vs `--num-tokens`, the three MoE cache sizing flags) are
//! declared alongside the knobs rather than hard-coded in the UI.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Group {
    Model,
    Server,
    Runtime,
    Memory,
    Moe,
    Api,
}

impl Group {
    pub const ALL: [Group; 6] =
        [Group::Model, Group::Server, Group::Runtime, Group::Memory, Group::Moe, Group::Api];

    pub fn title(self) -> &'static str {
        match self {
            Group::Model => "Model",
            Group::Server => "Server",
            Group::Runtime => "Runtime & scheduling",
            Group::Memory => "KV cache & memory",
            Group::Moe => "MoE offload",
            Group::Api => "API behavior",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Kind {
    /// Free text (paths, host names, layer specs).
    Text,
    /// Whole number, optionally bounded.
    Int { min: Option<i64>, max: Option<i64> },
    /// Real number in a closed range.
    Float { min: f64, max: f64 },
    /// A `store_true` switch; emitted as a bare flag when on.
    Flag,
    /// One of a fixed set. The first entry is the "leave it to FreeToken" choice where
    /// one exists (`auto`), and is what an unset knob resolves to.
    Choice(&'static [&'static str]),
}

pub struct Knob {
    /// Stable identifier used in profiles and on the wire to the UI.
    pub key: &'static str,
    /// The flag as `ft serve` spells it.
    pub flag: &'static str,
    pub label: &'static str,
    pub group: Group,
    pub kind: Kind,
    /// What FreeToken does when the flag is absent. Shown as the placeholder.
    pub default: &'static str,
    pub help: &'static str,
    /// Keys that cannot be set at the same time as this one.
    pub exclusive_with: &'static [&'static str],
}

macro_rules! knob {
    ($key:literal, $flag:literal, $label:literal, $group:expr, $kind:expr, $default:literal, $help:literal $(, excl = $excl:expr)? $(,)?) => {
        Knob {
            key: $key,
            flag: $flag,
            label: $label,
            group: $group,
            kind: $kind,
            default: $default,
            help: $help,
            exclusive_with: knob!(@excl $($excl)?),
        }
    };
    (@excl) => { &[] };
    (@excl $excl:expr) => { $excl };
}

const MOE_CACHE_EXCL: &[&str] = &["moe_cache_size", "moe_cache_rate", "moe_cache_auto"];
const KV_CAP_EXCL: &[&str] = &["num_pages", "num_tokens"];

/// Every knob, in the order the Config view presents them within each group.
pub static KNOBS: &[Knob] = &[
    // ---- Model -----------------------------------------------------------
    knob!("model", "--model", "Model path or repo id", Group::Model, Kind::Text, "(required)",
        "Local directory, Hugging Face repo id, or an FTW directory. FTW is auto-detected."),
    knob!("served_model_name", "--served-model-name", "Served model name", Group::Model, Kind::Text,
        "basename of --model", "The id reported by /v1/models and expected in request bodies."),
    knob!("dtype", "--dtype", "Weight dtype", Group::Model,
        Kind::Choice(&["auto", "bfloat16", "float16", "float32"]), "auto",
        "Data type for weights and activations. 'auto' takes the checkpoint's own dtype."),
    knob!("model_source", "--model-source", "Download source", Group::Model,
        Kind::Choice(&["huggingface", "modelscope"]), "huggingface",
        "Where a bare repo id is fetched from when --model is not a local path."),

    // ---- Server ----------------------------------------------------------
    knob!("host", "--host", "Bind address", Group::Server, Kind::Text, "127.0.0.1",
        "Interface the API server binds. Use 0.0.0.0 to expose it beyond localhost."),
    knob!("port", "--port", "Bind port", Group::Server,
        Kind::Int { min: Some(1), max: Some(65535) }, "1919",
        "TCP port for the OpenAI, Anthropic and Responses APIs."),
    knob!("gpu", "--gpu", "GPU", Group::Server, Kind::Text, "GPU 0",
        "A GPU UUID (GPU-xxxx..., as nvidia-smi -L prints) or an nvidia-smi index. A unique UUID prefix is enough."),
    knob!("cors_origins", "--cors-origins", "CORS origins", Group::Server, Kind::Text,
        "local Tauri/Vite origins",
        "Comma-separated allow-list for browser clients. Empty disables CORS headers; '*' allows any origin."),

    // ---- Runtime ---------------------------------------------------------
    knob!("max_running_requests", "--max-running-requests", "Max running requests", Group::Runtime,
        Kind::Int { min: Some(1), max: None }, "4",
        "How many requests the scheduler runs concurrently. Raising it costs KV cache."),
    knob!("max_output_tokens", "--max-output-tokens", "Default max output tokens", Group::Runtime,
        Kind::Int { min: Some(1), max: None }, "32768",
        "Output budget applied to requests that do not specify one."),
    knob!("max_seq_len_override", "--max-seq-len-override", "Max sequence length", Group::Runtime,
        Kind::Int { min: Some(1), max: None }, "from checkpoint",
        "Override the context length taken from the checkpoint's rotary config."),
    knob!("max_prefill_length", "--max-prefill-length", "Prefill chunk size", Group::Runtime,
        Kind::Int { min: Some(1), max: None }, "8192",
        "Chunked-prefill chunk size in tokens. Larger chunks prefill faster but delay decode."),
    knob!("cuda_graph_max_bs", "--cuda-graph-max-bs", "CUDA graph max batch", Group::Runtime,
        Kind::Int { min: Some(1), max: None }, "= max running requests",
        "Largest batch size captured as a CUDA graph. Capture cost grows with the value."),
    knob!("decode_log_interval", "--decode-log-interval", "Decode log interval", Group::Runtime,
        Kind::Int { min: Some(1), max: None }, "40",
        "Emit one scheduler status line every N decode forwards."),
    knob!("num_tokenizer", "--num-tokenizer", "Tokenizer processes", Group::Runtime,
        Kind::Int { min: Some(0), max: None }, "0",
        "Dedicated tokenizer processes. 0 shares the tokenizer with the detokenizer."),
    knob!("tensor_parallel_size", "--tensor-parallel-size", "Tensor parallel size", Group::Runtime,
        Kind::Int { min: Some(1), max: None }, "1",
        "TP degree. Give one --gpu entry per rank; single-GPU serving leaves this at 1."),
    knob!("disable_pynccl", "--disable-pynccl", "Disable PyNCCL", Group::Runtime, Kind::Flag, "off",
        "Turn off PyNCCL for tensor parallelism and fall back to the default collectives."),
    knob!("enable_special_token_ckpt", "--enable-special-token-ckpt", "Special-token checkpoints",
        Group::Runtime, Kind::Flag, "off",
        "Preserve a decode reuse point just after a tool-call opener, so rewriting an echoed tool call only invalidates the call body."),

    // ---- KV cache & memory ----------------------------------------------
    knob!("memory_ratio", "--memory-ratio", "Memory ratio", Group::Memory,
        Kind::Float { min: 0.05, max: 1.0 }, "0.9",
        "Fraction of free VRAM the engine may use for weights, MoE cache and KV combined."),
    knob!("num_pages", "--num-pages", "KV capacity (pages)", Group::Memory,
        Kind::Int { min: Some(1), max: None }, "auto",
        "Fix KV capacity in pages instead of sizing it from leftover VRAM.",
        excl = KV_CAP_EXCL),
    knob!("num_tokens", "--num-tokens", "KV capacity (tokens)", Group::Memory,
        Kind::Int { min: Some(1), max: None }, "auto",
        "Fix KV capacity in tokens. Must be a multiple of the resolved page size.",
        excl = KV_CAP_EXCL),
    knob!("page_size", "--page-size", "KV page size", Group::Memory,
        Kind::Int { min: Some(1), max: None }, "1",
        "KV page granularity. DSV4 forces 128, the TRTLLM backend needs 16/32/64, SWA models require 1."),
    knob!("cache_type", "--cache-type", "KV cache strategy", Group::Memory,
        Kind::Choice(&["radix", "naive"]), "radix",
        "'radix' reuses shared prefixes across requests (SWA- and GDN-aware variants are picked automatically); 'naive' opts out."),
    knob!("attention_backend", "--attention-backend", "Attention backend", Group::Memory,
        Kind::Choice(&["auto", "trtllm", "fi", "fa", "triton", "dsv4_sparse", "dsa"]), "auto",
        "Attention kernel. A 'prefill,decode' pair is also accepted; type it in the free-text override if you need one."),
    knob!("kv_reserve_tokens", "--kv-reserve-tokens", "KV reserve tokens", Group::Memory,
        Kind::Int { min: Some(0), max: None }, "8192",
        "KV token floor held back before --moe-cache-auto spends the rest of VRAM on experts."),

    // ---- MoE -------------------------------------------------------------
    // Recent FreeToken renamed this to --moe-strategy and keeps --moe-backend as a warning
    // alias; older builds only know --moe-backend, so that is what ft-man emits.
    knob!("moe_backend", "--moe-backend", "MoE strategy", Group::Moe,
        Kind::Choice(&["auto", "offload", "hybrid", "cpu", "fused"]), "auto",
        "fused keeps experts resident on GPU; offload streams misses over PCIe; cpu computes them on the host; hybrid splits the two. auto never picks fused. Newer FreeToken spells this --moe-strategy."),
    knob!("moe_cache_size", "--moe-cache-size", "MoE cache size (slots)", Group::Moe,
        Kind::Int { min: Some(0), max: None }, "auto",
        "Absolute number of GPU expert slots.", excl = MOE_CACHE_EXCL),
    knob!("moe_cache_rate", "--moe-cache-rate", "MoE cache rate", Group::Moe,
        Kind::Float { min: 0.0, max: 1.0 }, "auto",
        "Fraction of all experts to keep cached on GPU.", excl = MOE_CACHE_EXCL),
    knob!("moe_cache_auto", "--moe-cache-auto", "MoE cache auto-size", Group::Moe, Kind::Flag,
        "on for offload-family backends",
        "Size the expert cache from free VRAM, giving MoE priority over KV down to the reserve floor.",
        excl = MOE_CACHE_EXCL),
    knob!("moe_cache_policy", "--moe-cache-policy", "MoE eviction policy", Group::Moe,
        Kind::Choice(&["lru"]), "lru", "Eviction policy for the unified expert slot cache."),
    knob!("moe_cpu_threads", "--moe-cpu-threads", "MoE CPU threads", Group::Moe,
        Kind::Int { min: Some(0), max: None }, "physical cores",
        "Worker threads for the cpu and hybrid executors. 0 means one per physical core."),
    knob!("moe_cpu_layers", "--moe-cpu-layers", "MoE CPU layers", Group::Moe, Kind::Text,
        "every layer on GPU",
        "With offload or hybrid: which MoE layers decode on the CPU. An id list ('3,7,11'), a count ('8'), a fraction ('0.5'), or 'auto'. 'auto' is for Windows/WSL, where CUDA pinned memory is capped, and needs an expert format the CPU executor serves (bf16, nvfp4, mxfp4) -- not fp8."),
    knob!("moe_hybrid_max_fetch", "--moe-hybrid-max-fetch", "Hybrid max fetch", Group::Moe,
        Kind::Int { min: Some(-1), max: None }, "-1 (auto)",
        "With hybrid: experts fetched over PCIe per layer per step; the rest go to the CPU. -1 reads the bandwidth profile, 0 never fetches."),
    // Newer FreeToken folds this into --quant-backend (moe.nvfp4=<marlin|b12x|triton>, where
    // b12x is the old flashinfer) and keeps --nvfp4-backend as a warning alias. The two are
    // mutually exclusive there, so a --quant-backend knob must exclude this one.
    knob!("nvfp4_backend", "--nvfp4-backend", "NVFP4 GEMM backend", Group::Moe,
        Kind::Choice(&["triton", "auto", "marlin", "flashinfer"]), "triton",
        "Routed-expert GEMM kernel for NVFP4 checkpoints. Forcing one fails loudly if it cannot run. Newer FreeToken spells this --quant-backend moe.nvfp4=<kernel>."),
    knob!("expert_load", "--expert-load", "Expert bank load", Group::Moe,
        Kind::Choice(&["auto", "parallel", "serial"]), "auto",
        "How expert banks are read into host RAM. 'serial' is the low-memory path; 'parallel' is faster but needs room for a whole-shard buffer."),
    knob!("ple_backend", "--ple-backend", "PLE table backend", Group::Moe,
        Kind::Choice(&["disk", "pinned"]), "disk",
        "Where a PLE n-gram table lives. 'pinned' preloads it into page-locked host RAM, which is fast but costs tens of GiB."),
    knob!("moe_prefill_hit_d2d", "--moe-prefill-hit-d2d", "Prefill hit D2D copy", Group::Moe,
        Kind::Flag, "off",
        "During prefill, copy cache-resident experts device-side and stream only misses. Needs CUDA 13 and a cache larger than 2x the per-layer expert count."),
    knob!("disable_moe_prefill_overlap", "--disable-moe-prefill-overlap", "Disable prefill overlap",
        Group::Moe, Kind::Flag, "overlap on",
        "Turn off the two-buffer overlap for prefill expert copies."),

    // ---- API behavior ----------------------------------------------------
    knob!("tool_call_parser", "--tool-call-parser", "Tool-call parser", Group::Api,
        Kind::Choice(&["auto", "llama3", "qwen", "qwen25", "qwen3_coder", "mistral", "deepseekv32",
                       "gemma4", "glm47", "minimax", "minimax_m3", "muse_glimmer", "gpt_oss"]),
        "auto", "Grammar used to parse tool calls out of the model's output. 'auto' infers it from the model family."),
    knob!("reasoning_parser", "--reasoning-parser", "Reasoning parser", Group::Api,
        Kind::Choice(&["auto", "off", "qwen3", "glm", "deepseekv32", "gpt_oss", "minimax",
                       "minimax_m3", "muse_glimmer", "gemma4"]),
        "auto", "Splits chain-of-thought into reasoning_content. 'off' leaves it inline in the message."),
    knob!("sampling_defaults", "--sampling-defaults", "Sampling defaults", Group::Api,
        Kind::Choice(&["model", "none"]), "model",
        "'model' fills unspecified temperature/top_k/top_p from the checkpoint's generation_config.json, which reasoning models generally need."),
    knob!("enable_cache_report", "--enable-cache-report", "Report cache hits", Group::Api,
        Kind::Flag, "off",
        "Report prefix-cache hits in each response's usage block. On /v1/messages this also makes input_tokens exclude the cached prefix."),
];

pub fn knob(key: &str) -> Option<&'static Knob> {
    KNOBS.iter().find(|k| k.key == key)
}

pub fn knobs_in(group: Group) -> impl Iterator<Item = &'static Knob> {
    KNOBS.iter().filter(move |k| k.group == group)
}

// ---------------------------------------------------------------- values

/// An edited set of knob values. Only knobs the user actually set are stored, so a
/// profile records intent ("leave the MoE backend on auto") rather than a snapshot of
/// today's defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ServeConfig {
    values: BTreeMap<String, String>,
}

impl ServeConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    pub fn is_set(&self, key: &str) -> bool {
        self.values.contains_key(key)
    }

    /// True when a `Flag` knob is on.
    pub fn flag(&self, key: &str) -> bool {
        matches!(self.values.get(key).map(String::as_str), Some("true"))
    }

    /// Set a knob, clearing anything it is mutually exclusive with. An empty value
    /// unsets the knob instead, which is how the UI clears a field.
    pub fn set(&mut self, key: &str, value: impl Into<String>) {
        let value = value.into();
        if value.is_empty()
            || value == "false" && matches!(knob(key).map(|k| k.kind), Some(Kind::Flag))
        {
            self.values.remove(key);
            return;
        }
        if let Some(k) = knob(key) {
            for other in k.exclusive_with {
                if *other != key {
                    self.values.remove(*other);
                }
            }
        }
        self.values.insert(key.to_string(), value);
    }

    pub fn unset(&mut self, key: &str) {
        self.values.remove(key);
    }

    pub fn toggle_flag(&mut self, key: &str) {
        if self.flag(key) {
            self.unset(key);
        } else {
            self.set(key, "true");
        }
    }

    /// Build the `ft serve` argument list. Knobs are emitted in schema order so the
    /// preview and the real invocation always read the same way.
    pub fn to_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        for k in KNOBS {
            let Some(value) = self.values.get(k.key) else { continue };
            match k.kind {
                Kind::Flag => {
                    if value == "true" {
                        args.push(k.flag.to_string());
                    }
                }
                _ => {
                    if value.is_empty() {
                        continue;
                    }
                    args.push(k.flag.to_string());
                    args.push(value.clone());
                }
            }
        }
        args
    }

    /// A shell-quoted preview of the full command line.
    pub fn preview(&self, program: &str) -> String {
        let mut parts = vec![program.to_string(), "serve".to_string()];
        parts.extend(self.to_args());
        parts
            .iter()
            .map(|p| {
                if p.chars().any(|c| c.is_whitespace() || "\"'$`\\|&;<>()*?[]{}~#!".contains(c)) {
                    format!("'{}'", p.replace('\'', r"'\''"))
                } else {
                    p.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Validate every set value against its knob. Returns `(key, message)` pairs.
    pub fn validate(&self) -> Vec<(String, String)> {
        let mut errors = Vec::new();
        if self.get("model").map(str::trim).unwrap_or("").is_empty() {
            errors.push(("model".into(), "a model path or repo id is required".into()));
        }
        for (key, value) in &self.values {
            let Some(k) = knob(key) else {
                errors.push((key.clone(), "unknown knob".into()));
                continue;
            };
            if let Some(msg) = validate_value(k, value) {
                errors.push((key.clone(), msg));
            }
            for other in k.exclusive_with {
                if *other != key.as_str() && self.values.contains_key(*other) {
                    let other_flag = knob(other).map(|k| k.flag).unwrap_or(other);
                    errors.push((key.clone(), format!("cannot be combined with {other_flag}")));
                }
            }
        }
        errors.sort();
        errors.dedup();
        errors
    }
}

/// Check one value against a knob's declared domain. `None` means it is fine.
pub fn validate_value(k: &Knob, value: &str) -> Option<String> {
    let v = value.trim();
    if v.is_empty() {
        return None;
    }
    match k.kind {
        Kind::Text => None,
        Kind::Flag => (v != "true" && v != "false").then(|| "must be true or false".to_string()),
        Kind::Int { min, max } => {
            let Ok(n) = v.parse::<i64>() else {
                return Some("must be a whole number".into());
            };
            if let Some(lo) = min {
                if n < lo {
                    return Some(format!("must be at least {lo}"));
                }
            }
            if let Some(hi) = max {
                if n > hi {
                    return Some(format!("must be at most {hi}"));
                }
            }
            None
        }
        Kind::Float { min, max } => {
            let Ok(x) = v.parse::<f64>() else {
                return Some("must be a number".into());
            };
            (!(min..=max).contains(&x)).then(|| format!("must be between {min} and {max}"))
        }
        Kind::Choice(options) => {
            (!options.contains(&v)).then(|| format!("must be one of: {}", options.join(", ")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_knob_key_is_unique() {
        let mut keys: Vec<_> = KNOBS.iter().map(|k| k.key).collect();
        let total = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), total, "duplicate knob key");
    }

    #[test]
    fn exclusive_groups_reference_real_knobs() {
        for k in KNOBS {
            for other in k.exclusive_with {
                assert!(knob(other).is_some(), "{} excludes unknown knob {other}", k.key);
            }
        }
    }

    #[test]
    fn setting_a_moe_cache_knob_clears_its_siblings() {
        let mut cfg = ServeConfig::new();
        cfg.set("moe_cache_rate", "0.5");
        cfg.set("moe_cache_size", "128");
        assert_eq!(cfg.get("moe_cache_size"), Some("128"));
        assert!(!cfg.is_set("moe_cache_rate"));
        assert!(cfg.validate().iter().all(|(k, _)| k != "moe_cache_size"));
    }

    #[test]
    fn args_are_emitted_in_schema_order() {
        let mut cfg = ServeConfig::new();
        cfg.set("port", "1920");
        cfg.set("model", "/models/qwen");
        cfg.set("enable_cache_report", "true");
        assert_eq!(
            cfg.to_args(),
            vec!["--model", "/models/qwen", "--port", "1920", "--enable-cache-report"]
        );
    }

    #[test]
    fn out_of_range_values_are_reported() {
        let mut cfg = ServeConfig::new();
        cfg.set("model", "x");
        cfg.set("memory_ratio", "1.5");
        let errs = cfg.validate();
        assert!(errs.iter().any(|(k, _)| k == "memory_ratio"), "{errs:?}");
    }
}
