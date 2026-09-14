//! The `ft serve` knob schema and the editable configuration built on top of it.
//!
//! Every flag FreeToken's server accepts is described once, here, with its type, domain,
//! default and help text. The Config view renders straight from this table, validation
//! reads it, and [`ServeConfig::to_args`] turns an edited set back into an argv. Mutual
//! exclusions (`--num-pages` vs `--num-tokens`, the three MoE cache sizing flags) are
//! declared alongside the knobs rather than hard-coded in the UI.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Group {
    Model,
    Server,
    Runtime,
    Memory,
    Moe,
    Multimodal,
    Api,
}

impl Group {
    pub const ALL: [Group; 7] = [
        Group::Model,
        Group::Server,
        Group::Runtime,
        Group::Memory,
        Group::Moe,
        Group::Multimodal,
        Group::Api,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Group::Model => "Model",
            Group::Server => "Server",
            Group::Runtime => "Runtime & scheduling",
            Group::Memory => "KV cache & memory",
            Group::Moe => "MoE offload",
            Group::Multimodal => "Multimodal",
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
    /// Any subset of a fixed set, stored space-separated and emitted as one flag followed
    /// by each chosen value — the shape argparse's `nargs="+"` reads.
    Multi(&'static [&'static str]),
}

#[derive(Serialize)]
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

// `{kind, ...}`, written out because `Choice` holds an unnamed slice that internal
// tagging cannot place, and the wire format names it `options`.
impl Serialize for Kind {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = s.serialize_map(None)?;
        match self {
            Kind::Text => m.serialize_entry("kind", "text")?,
            Kind::Flag => m.serialize_entry("kind", "flag")?,
            Kind::Int { min, max } => {
                m.serialize_entry("kind", "int")?;
                m.serialize_entry("min", min)?;
                m.serialize_entry("max", max)?;
            }
            Kind::Float { min, max } => {
                m.serialize_entry("kind", "float")?;
                m.serialize_entry("min", min)?;
                m.serialize_entry("max", max)?;
            }
            Kind::Choice(options) => {
                m.serialize_entry("kind", "choice")?;
                m.serialize_entry("options", options)?;
            }
            Kind::Multi(options) => {
                m.serialize_entry("kind", "multi")?;
                m.serialize_entry("options", options)?;
            }
        }
        m.end()
    }
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
        Kind::Choice(&["auto", "trtllm", "fi", "fa", "triton", "dsv4_sparse", "dsa", "m3_sparse",
                       "qsa_sparse"]), "auto",
        "Attention kernel. Each backend serves one attention type, so 'auto' is almost always right: m3_sparse is MiniMax-M3's block-sparse kernel, qsa_sparse Qwen3.8-Flash-Next's, dsa and dsv4_sparse DeepSeek's."),
    knob!("kv_reserve_tokens", "--kv-reserve-tokens", "KV reserve tokens", Group::Memory,
        Kind::Int { min: Some(0), max: None }, "8192",
        "KV token floor held back before --moe-cache-auto spends the rest of VRAM on experts."),

    // ---- MoE -------------------------------------------------------------
    knob!("moe_strategy", "--moe-strategy", "MoE strategy", Group::Moe,
        Kind::Choice(&["auto", "offload", "hybrid", "cpu", "fused"]), "auto",
        "fused keeps experts resident on GPU; offload streams misses over PCIe; cpu computes them on the host; hybrid splits the two. auto never picks fused."),
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
    // Replaced --nvfp4-backend, which reached only the routed-expert NVFP4 table. Kept in
    // this group because that table is still what it is overwhelmingly used for, even
    // though the flag also reaches the dense linear kernels.
    knob!("quant_backend", "--quant-backend", "Quant kernels", Group::Moe, Kind::Text,
        "automatic per table",
        "Which kernel serves each quantized layer type: comma-separated layer[.kind]=name entries, e.g. 'moe.nvfp4=marlin' or 'linear=triton'. A bare layer applies to every one of its kinds whose table lists that kernel; unlisted tables stay automatic. Forcing a kernel fails loudly if it cannot run here."),
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
    // ---- Multimodal ------------------------------------------------------
    // Every family that registers a vision encoder serves image input by default — the Qwen
    // families, and Gemma-4 since #467 — and the encoder tower built for it comes out of the
    // same VRAM the KV and expert pools are priced against. That makes this group a memory
    // group as much as an API one, which is why it sits beside the MoE knobs rather than
    // under "API behavior". The flags are read in each family's own units, so the help below
    // describes what they mean rather than quoting one family's numbers.
    knob!("text_model_only", "--text-model-only", "Text only", Group::Multimodal,
        Kind::Flag, "off",
        "Serve a multimodal checkpoint without its encoder towers: none are built, the VRAM they \
         would hold goes to the KV and expert pools instead, and every image request is rejected. \
         The lever to reach for when a vision-capable checkpoint is being served for text."),
    knob!("mm_disable", "--mm-disable", "Disable encoders", Group::Multimodal,
        Kind::Multi(&["vision", "audio"]), "none",
        "Encoder towers to leave unbuilt, chosen one kind at a time; every input they would serve \
         is rejected. Naming every kind is exactly what --text-model-only does."),
    knob!("mm_encoder_weights", "--mm-encoder-weights", "Encoder weights", Group::Multimodal,
        Kind::Choice(&["host", "gpu"]), "host",
        "Where the encoder tower's block weights live. 'host' streams them from pinned host banks \
         two blocks at a time behind the compute, so the GPU holds two blocks instead of the whole \
         tower: small images pay the copy time, large ones hide it. 'gpu' keeps them resident. An \
         encoder with no block stack stays resident either way."),
    knob!("image_min_tokens", "--image-min-tokens", "Image min tokens", Group::Multimodal,
        Kind::Int { min: Some(1), max: None }, "the processor's own limit",
        "Fewest tokens one image may take; smaller images are scaled up to it. Each family reads \
         this in its own units — a dynamic-resolution family as a pixel area, and a family with \
         fixed budgets not at all, since it has nothing between its budgets to choose."),
    knob!("image_max_tokens", "--image-max-tokens", "Image max tokens", Group::Multimodal,
        Kind::Int { min: Some(1), max: None }, "the processor's own limit",
        "Most tokens one image may take, and so the cap on what one image costs in prefill. A \
         family with fixed budgets picks the largest budget within it and refuses at start-up a \
         maximum below its smallest — the one value here that can stop a serve from starting."),
    knob!("mm_processor_kwargs", "--mm-processor-kwargs", "Processor kwargs", Group::Multimodal,
        Kind::Text, "none",
        "JSON object of extra keyword arguments for the checkpoint's image processor, for knobs \
         the token budget does not cover — {\"size\": {\"longest_edge\": 1048576}} for a \
         dynamic-resolution family, {\"max_soft_tokens\": 1120} for a fixed-budget one. Applied \
         after the budget, so an explicit key wins."),
    knob!("mm_embed_cache_device", "--mm-embed-cache-device", "Embedding cache", Group::Multimodal,
        Kind::Choice(&["cpu", "cuda"]), "cpu",
        "Where encoded image embeddings wait between prefill chunks. 'cpu' keeps them out of the \
         VRAM budget; 'cuda' skips the copy back."),
    knob!("allowed_media_domains", "--allowed-media-domains", "Allowed media domains",
        Group::Multimodal, Kind::Text, "any",
        "Comma-separated hostname allowlist for the image URLs a client may send; anything else is \
         refused with a 400. Empty admits any domain, which means the server will fetch whatever a \
         request names."),
    knob!("allowed_local_media_path", "--allowed-local-media-path", "Local media path",
        Group::Multimodal, Kind::Text, "off",
        "Directory that file:// image references may be read from. Unset rejects local files \
         outright; FreeToken checks the directory exists and refuses to start if it does not."),

    knob!("sampling_defaults", "--sampling-defaults", "Sampling defaults", Group::Api,
        Kind::Choice(&["model", "none"]), "model",
        "'model' fills unspecified temperature/top_k/top_p from the checkpoint's generation_config.json, which reasoning models generally need."),
    knob!("enable_cache_report", "--enable-cache-report", "Report cache hits", Group::Api,
        Kind::Flag, "off",
        "Report prefix-cache hits in each response's usage block. On /v1/messages this also makes input_tokens exclude the cached prefix."),
];

/// The kernel tables `--quant-backend` can name: `(layer, kind, kernels)`, mirroring the
/// `candidates` lists in FreeToken's `layers/quantization/{linear,moe}/*.py`. The order is
/// FreeToken's own, which is also its auto-selection order: the first kernel that can run
/// here and is worth it wins, so naming one only ever overrides that search.
///
/// `moe.mxfp8` is deliberately absent: its method registers an empty candidate table, so
/// FreeToken accepts no kernel name for it either.
static QUANT_TABLES: &[(&str, &str, &[&str])] = &[
    ("linear", "none", &["torch"]),
    ("linear", "fp8_tensor", &["torch", "triton", "emulation"]),
    ("linear", "fp8_block", &["dsv4", "triton"]),
    ("linear", "mxfp8", &["triton", "emulation"]),
    ("linear", "nvfp4", &["triton", "marlin", "emulation"]),
    ("moe", "none", &["fused"]),
    ("moe", "fp8_block", &["triton"]),
    ("moe", "mxfp4", &["triton", "triton_gptoss"]),
    ("moe", "nvfp4", &["triton", "marlin", "b12x"]),
];

/// Every kernel name a `layer[.kind]` key accepts. A bare layer takes the union of its
/// kinds, which is how FreeToken resolves a layer-wide entry.
fn quant_kernels(layer: &str, kind: Option<&str>) -> Vec<&'static str> {
    // First occurrence wins, so the union keeps FreeToken's search order; a plain `dedup`
    // would not, because the same kernel serves several kinds without being adjacent.
    let mut names: Vec<&'static str> = Vec::new();
    for kernel in QUANT_TABLES
        .iter()
        .filter(|(l, k, _)| *l == layer && kind.is_none_or(|want| *k == want))
        .flat_map(|(_, _, kernels)| kernels.iter().copied())
    {
        if !names.contains(&kernel) {
            names.push(kernel);
        }
    }
    names
}

/// Check one `--quant-backend` value, mirroring `QuantBackend.parse`. FreeToken rejects a
/// bad entry with a usage error before it does any work, so catching it here turns a
/// failed launch into a red field.
fn validate_quant_backend(value: &str) -> Option<String> {
    for item in value.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let Some((key, name)) = item.split_once('=') else {
            return Some(format!("{item:?} is not layer[.kind]=name"));
        };
        let (layer, kind) = match key.trim().split_once('.') {
            Some((l, k)) => (l.trim(), Some(k.trim())),
            None => (key.trim(), None),
        };
        if layer != "linear" && layer != "moe" {
            return Some(format!("no layer {layer:?}; expected linear or moe"));
        }
        let kernels = quant_kernels(layer, kind);
        if kernels.is_empty() {
            return Some(format!("{layer} has no {} table", kind.unwrap_or_default()));
        }
        let name = name.trim().to_lowercase();
        if !kernels.contains(&name.as_str()) {
            return Some(format!("no {key} kernel {name:?}; known: {}", kernels.join(", ")));
        }
    }
    None
}

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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ServeConfig {
    values: BTreeMap<String, String>,
}

impl<'de> Deserialize<'de> for ServeConfig {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Self { values: migrate(BTreeMap::deserialize(d)?) })
    }
}

/// Carry a profile written against an older FreeToken forward onto the current flag
/// names. ft-man follows the CLI it is installed next to rather than supporting several
/// at once, so a renamed flag has to be translated on the way in — otherwise loading an
/// existing profile would quietly drop the setting as an unknown knob.
fn migrate(mut values: BTreeMap<String, String>) -> BTreeMap<String, String> {
    // --moe-backend became --moe-strategy in FreeToken #418, with the same value space.
    if let Some(v) = values.remove("moe_backend") {
        values.entry("moe_strategy".into()).or_insert(v);
    }
    // --nvfp4-backend became a single --quant-backend entry in the same release. Mirrors
    // FreeToken's own `_nvfp4_entry`: "auto" stood for "no entry at all", and the kernel
    // that was called flashinfer is now called b12x.
    if let Some(v) = values.remove("nvfp4_backend") {
        let kernel = match v.as_str() {
            "auto" => "",
            "flashinfer" => "b12x",
            other => other,
        };
        if !kernel.is_empty() {
            values.entry("quant_backend".into()).or_insert(format!("moe.nvfp4={kernel}"));
        }
    }
    values
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
                // `nargs="+"`: the flag once, then each value as its own argv element.
                Kind::Multi(_) => {
                    let mut values = value.split_whitespace().peekable();
                    if values.peek().is_none() {
                        continue;
                    }
                    args.push(k.flag.to_string());
                    args.extend(values.map(str::to_string));
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
        // FreeToken rejects this pair at startup; catching it here means the Serve tab says
        // so while it can still be fixed, rather than after a launch that dies.
        if let (Some(lo), Some(hi)) = (self.get("image_min_tokens"), self.get("image_max_tokens")) {
            if let (Ok(lo), Ok(hi)) = (lo.trim().parse::<i64>(), hi.trim().parse::<i64>()) {
                if lo > hi {
                    errors.push((
                        "image_min_tokens".into(),
                        format!("{lo} is more than --image-max-tokens {hi}"),
                    ));
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
    if k.key == "quant_backend" {
        return validate_quant_backend(v);
    }
    // FreeToken parses this one with json.loads and refuses to start on anything that is
    // not an object, so it is worth catching here rather than in a failed launch.
    if k.key == "mm_processor_kwargs" {
        return match serde_json::from_str::<serde_json::Value>(v) {
            Ok(serde_json::Value::Object(_)) => None,
            Ok(_) => Some("must be a JSON object".into()),
            Err(e) => Some(format!("not valid JSON: {e}")),
        };
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
        Kind::Multi(options) => {
            let mut chosen: Vec<&str> = Vec::new();
            for token in v.split_whitespace() {
                if !options.contains(&token) {
                    return Some(format!("must be one or more of: {}", options.join(", ")));
                }
                if chosen.contains(&token) {
                    return Some(format!("{token} is named twice"));
                }
                chosen.push(token);
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A choice knob accepts exactly its declared options and nothing else. The Serve
    /// tab only ever cycles through them, so the value that reaches this is one a browser
    /// or a hand-edited profile supplied.
    #[test]
    fn a_choice_knob_accepts_only_its_own_options() {
        let k = knob("moe_strategy").expect("the MoE strategy knob exists");
        let Kind::Choice(options) = k.kind else { panic!("moe_strategy is a choice knob") };
        for good in options {
            assert_eq!(validate_value(k, good), None, "{good} is one of its options");
        }
        // Trimmed before comparison, as every other kind is.
        assert_eq!(validate_value(k, &format!("  {}  ", options[0])), None);

        let msg = validate_value(k, "turbo").expect("an unlisted option must be rejected");
        assert!(msg.starts_with("must be one of: "), "{msg}");
        for good in options {
            assert!(msg.contains(good), "the message lists what is allowed: {msg}");
        }
        // Case matters: these are argv values, and FreeToken compares them exactly.
        assert!(validate_value(k, &options[0].to_uppercase()).is_some());
        // An empty value is an unset knob, not a bad one.
        assert_eq!(validate_value(k, ""), None);
    }

    /// A flag's domain is the two words `ServeConfig::set` understands. "false" is valid
    /// and means "unset", which is why it has to be accepted rather than rejected.
    #[test]
    fn a_flag_accepts_only_true_or_false() {
        let k = knob("moe_cache_auto").expect("the MoE auto flag exists");
        assert_eq!(validate_value(k, "true"), None);
        assert_eq!(validate_value(k, "false"), None);
        assert_eq!(validate_value(k, "yes").as_deref(), Some("must be true or false"));
    }

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
    fn a_quant_backend_entry_is_checked_against_the_real_kernel_tables() {
        let k = knob("quant_backend").unwrap();
        for good in [
            "moe.nvfp4=marlin",
            "moe.nvfp4=b12x",
            "moe.mxfp4=triton_gptoss",
            "linear.fp8_block=dsv4",
            "linear=marlin",
            "linear=marlin,moe.nvfp4=triton",
        ] {
            assert_eq!(validate_value(k, good), None, "{good} should be accepted");
        }
        // b12x is a routed-expert kernel; the dense linear table has no such entry.
        assert!(validate_value(k, "linear=b12x").is_some());
        // "flashinfer" was the old --nvfp4-backend spelling of b12x and is gone.
        assert!(validate_value(k, "moe.nvfp4=flashinfer").is_some());
        // moe.mxfp8 registers an empty candidate table, so no name is valid for it.
        assert!(validate_value(k, "moe.mxfp8=triton").is_some());
        assert!(validate_value(k, "attention=fa").is_some());
        assert!(validate_value(k, "moe.nvfp4").is_some());
        // A layer-wide key names the union of its kinds, each kernel once and in
        // FreeToken's own search order.
        assert_eq!(
            validate_value(k, "linear=nope").as_deref(),
            Some("no linear kernel \"nope\"; known: torch, triton, emulation, dsv4, marlin"),
        );
    }

    /// A profile saved against the pre-#418 CLI still has to load with its settings
    /// intact, because the knob keys it names no longer exist.
    #[test]
    fn a_profile_written_against_the_old_flag_names_is_carried_forward() {
        let old: ServeConfig =
            toml::from_str("moe_backend = 'hybrid'\nnvfp4_backend = 'flashinfer'").unwrap();
        assert_eq!(old.get("moe_strategy"), Some("hybrid"));
        assert_eq!(old.get("quant_backend"), Some("moe.nvfp4=b12x"));
        assert!(!old.is_set("moe_backend"));
        assert!(old.validate().iter().all(|(k, _)| k != "moe_strategy" && k != "quant_backend"));

        // "auto" meant "let FreeToken choose", which is now simply an absent flag.
        let auto: ServeConfig = toml::from_str("nvfp4_backend = 'auto'").unwrap();
        assert!(!auto.is_set("quant_backend"));
    }

    #[test]
    fn out_of_range_values_are_reported() {
        let mut cfg = ServeConfig::new();
        cfg.set("model", "x");
        cfg.set("memory_ratio", "1.5");
        let errs = cfg.validate();
        assert!(errs.iter().any(|(k, _)| k == "memory_ratio"), "{errs:?}");
    }

    /// `--mm-disable` is argparse's `nargs="+"`: the flag once, then a separate argv
    /// element per value. Joining them into one word is the mistake this guards.
    #[test]
    fn a_multi_knob_emits_one_argv_element_per_value() {
        let mut c = ServeConfig::default();
        c.set("model", "/models/Qwen3.6-35B-A3B");
        c.set("mm_disable", "vision audio");
        let args = c.to_args();

        let at = args.iter().position(|a| a == "--mm-disable").expect("the flag is emitted");
        assert_eq!(&args[at + 1..at + 3], ["vision", "audio"]);
        // And not as one joined word, which argparse would take as a single bad choice.
        assert!(!args.iter().any(|a| a == "vision audio"), "{args:?}");
    }

    /// An empty selection is an unset knob: no flag at all, rather than a flag with no
    /// values, which argparse rejects outright.
    #[test]
    fn a_multi_knob_with_nothing_chosen_emits_no_flag() {
        let mut c = ServeConfig::default();
        c.set("model", "/models/Qwen3.6-35B-A3B");
        c.set("mm_disable", "   ");
        assert!(!c.to_args().iter().any(|a| a == "--mm-disable"));
    }

    /// Any subset in any order, but only from the declared set, and never twice.
    #[test]
    fn a_multi_knob_accepts_subsets_and_rejects_the_rest() {
        let k = knob("mm_disable").expect("the mm-disable knob exists");
        for good in ["vision", "audio", "vision audio", "audio vision", ""] {
            assert_eq!(validate_value(k, good), None, "{good:?} is a valid subset");
        }
        assert!(validate_value(k, "video").is_some(), "an unlisted kind is refused");
        let twice = validate_value(k, "vision vision").expect("a repeat is refused");
        assert!(twice.contains("twice"), "{twice}");
    }

    /// FreeToken parses `--mm-processor-kwargs` with `json.loads` and requires an object,
    /// so a typo should be caught on the Serve tab rather than by a launch that dies.
    #[test]
    fn processor_kwargs_must_be_a_json_object() {
        let k = knob("mm_processor_kwargs").expect("the processor-kwargs knob exists");
        assert_eq!(validate_value(k, r#"{"size": {"longest_edge": 1048576}}"#), None);
        assert_eq!(validate_value(k, "{}"), None);
        assert!(validate_value(k, "[1, 2]").is_some_and(|m| m.contains("JSON object")));
        assert!(validate_value(k, "size=1").is_some_and(|m| m.contains("not valid JSON")));
    }

    /// The one cross-knob check in this group: FreeToken refuses to start when the image
    /// token budget is inverted, and it should be visible before the launch.
    #[test]
    fn an_inverted_image_token_budget_is_an_error() {
        let mut c = ServeConfig::default();
        c.set("model", "/models/Qwen3.6-35B-A3B");
        c.set("image_min_tokens", "4096");
        c.set("image_max_tokens", "64");
        let errors = c.validate();
        assert!(
            errors.iter().any(|(key, msg)| key == "image_min_tokens" && msg.contains("more than")),
            "{errors:?}"
        );

        c.set("image_max_tokens", "16384");
        assert!(c.validate().is_empty(), "a budget the right way round is fine");
    }
}
