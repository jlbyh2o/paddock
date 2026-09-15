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

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    /// Worth knowing, not a problem.
    Info,
    /// Will work, but constrains how.
    Caution,
    /// Expected to fail.
    Blocker,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
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

#[derive(Debug, Clone, Default, Serialize)]
pub struct Report {
    pub arch: Option<String>,
    pub model_type: Option<String>,
    pub is_moe: bool,
    pub num_experts: Option<u64>,
    pub num_layers: Option<u64>,
    pub quant: Option<String>,
    pub context: Option<u64>,
    /// How this checkpoint caches, and what that costs per token. `None` when the config
    /// does not carry enough to price a row.
    pub kv: Option<KvGeometry>,
    /// The largest context this hardware can actually hold, clamped to the checkpoint's
    /// own ceiling. `None` when there is no hardware to price against, or when nothing in
    /// the model grows with the conversation and KV is therefore not the binding limit.
    pub max_servable_context: Option<u64>,
    /// True when the weights that would stay resident could not be priced — an offloaded
    /// MoE, or a listing whose size is not known yet — so `max_servable_context` is a
    /// ceiling the real configuration falls short of rather than a figure to plan on.
    pub context_is_upper_bound: bool,
    /// `(level, message)`, most severe first.
    #[serde(serialize_with = "notes_as_objects")]
    pub notes: Vec<(Level, String)>,
}

/// A note is a pair in Rust and `{level, text}` on the wire, because a two-element array
/// is unreadable in a browser's devtools and impossible to extend.
fn notes_as_objects<S: serde::Serializer>(
    notes: &[(Level, String)],
    s: S,
) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeSeq;
    let mut seq = s.serialize_seq(Some(notes.len()))?;
    for (level, text) in notes {
        seq.serialize_element(&Note { level: *level, text })?;
    }
    seq.end()
}

#[derive(Serialize)]
struct Note<'a> {
    level: Level,
    text: &'a str,
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
            parts.push(format!("{} ctx", crate::plan::tokens(c)));
        }
        // The per-token cost belongs in the one line two candidate repos are compared on:
        // it is what says a 256k ceiling is reachable here, and nothing else on this line
        // scales with the conversation.
        if let Some(kv) = &self.kv {
            parts.push(kv.per_token_label());
        }
        parts.join(" · ")
    }
}

/// The hardware a candidate would have to run on.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct Hardware {
    pub vram_bytes: u64,
    pub host_ram_bytes: u64,
    pub free_disk_bytes: u64,
}

// ---------------------------------------------------------------- KV geometry

/// FreeToken caches K and V in bf16 — two bytes an element. There is no KV dtype flag to
/// read (no `kv_cache_dtype` in the server's args or the engine's config), so this is a
/// constant rather than a guess. When one lands it becomes the only thing here that
/// changes, and every figure below halves for fp8.
const KV_BYTES_PER_ELEMENT: u64 = 2;

/// The share of the card FreeToken will use for weights, the expert cache and KV
/// together. Mirrors the default of `--memory-ratio`; `memory_ratio_matches_the_knob`
/// keeps the two from drifting.
const MEMORY_RATIO: f64 = 0.9;

/// Below this share of what a repo advertises, the gap stops being a detail and becomes
/// the reason not to download. Above it the figure is still worth stating — that is what
/// Info is for — but it is not a warning. Without some such line every modern checkpoint
/// would carry a caution on any consumer card, since almost none of them reach the 256k
/// and 1M ceilings they ship with, and a caution that is always on says nothing.
const KV_CAUTION_FRACTION: f64 = 0.25;

/// Layer kinds whose cache gains a row per token and never gives one back.
///
/// DeepSeek's sparse attention is here because its indexer selects from a full cache
/// rather than shrinking one: the reads are sparse, the storage is not.
const GROWING_TYPES: &[&str] = &["full_attention", "deepseek_sparse_attention"];

/// Layer kinds that cache rows but forget everything past their window.
const WINDOWED_TYPES: &[&str] = &["sliding_attention", "chunked_attention"];

/// Layer kinds that keep a fixed-size state per sequence instead of a row per token.
const FLAT_TYPES: &[&str] =
    &["linear_attention", "kda", "mamba", "mamba2", "gated_deltanet", "recurrent"];

/// The shape of one caching layer's per-token row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KvRow {
    /// Ordinary MHA/GQA: a key vector and a value vector for every KV head.
    Grouped { kv_heads: u64, head_dim: u64 },
    /// MLA (DeepSeek, GLM): one compressed latent for the whole layer rather than one
    /// vector per head, which is why an MLA checkpoint can cache an order of magnitude
    /// cheaper than a GQA one of the same size.
    Latent { width: u64 },
}

impl KvRow {
    fn bytes(self) -> u64 {
        match self {
            // Times two because a key row and a value row are both kept.
            KvRow::Grouped { kv_heads, head_dim } => 2 * kv_heads * head_dim * KV_BYTES_PER_ELEMENT,
            KvRow::Latent { width } => width * KV_BYTES_PER_ELEMENT,
        }
    }

    pub fn describe(self) -> String {
        match self {
            KvRow::Grouped { kv_heads, head_dim } => format!("{kv_heads} kv heads x {head_dim}"),
            KvRow::Latent { width } => format!("MLA latent {width}"),
        }
    }
}

/// How a checkpoint's layers cache, and what that costs per token.
///
/// The common intuition is that the KV cache is one buffer a conversation accumulates
/// into. It is not, and the difference is the whole point of reading this before a
/// download. A token is not attended to once; it is attended to once per layer, by that
/// layer's own attention, over that layer's own inputs. So each layer keeps its own key
/// and value row for every token it has seen, and layer 12's rows are not substitutable
/// for layer 30's — they are representations at different levels of abstraction. N
/// caching layers means N independent caches, each one row per token:
///
/// ```text
/// bytes/token = 2 (K and V) x layers that keep a growing cache x KV width x bytes/element
/// ```
///
/// Nothing in that expression is parameter count, which is why two 35B MoE checkpoints
/// can differ tenfold here. The multiplier that dominates is the first one — how many
/// layers keep a *growing* cache at all — and it is usually fewer than
/// `num_hidden_layers`, so it has to be derived rather than read.
///
/// This is the wall weights offload past and the cache cannot: a checkpoint whose weights
/// fit comfortably can still be unable to hold a tenth of its advertised context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KvGeometry {
    /// `num_hidden_layers`, kept for the "10 of 40" comparison that is the finding.
    pub layers: u64,
    /// Layers whose cache gains a row for every token, forever.
    pub growing_layers: u64,
    /// Sliding-window layers. They do keep rows, but forget everything past `window`, so
    /// their cost stops growing once the window is full.
    pub windowed_layers: u64,
    /// Linear-attention and state-space layers. Each folds every token into a fixed-size
    /// running state and overwrites it, so the cost is the same at one token as at 256k.
    pub flat_layers: u64,
    /// The shape of one caching layer's row.
    pub row: KvRow,
    /// Bytes one caching layer keeps per token.
    pub row_bytes: u64,
    /// `growing_layers x row_bytes` — the one figure that makes two candidate repos
    /// comparable at a glance, and the only one that scales with the conversation.
    pub bytes_per_token: u64,
    /// The sliding window's span, when any layer slides.
    pub window: Option<u64>,
}

impl KvGeometry {
    /// What the sliding-window layers cost at `context`: flat once the window is full.
    pub fn windowed_bytes(&self, context: u64) -> u64 {
        let rows = match self.window {
            Some(w) => context.min(w),
            None => context,
        };
        self.windowed_layers.saturating_mul(self.row_bytes).saturating_mul(rows)
    }

    /// Total KV bytes needed to hold `context` tokens.
    pub fn bytes_for_context(&self, context: u64) -> u64 {
        self.bytes_per_token.saturating_mul(context).saturating_add(self.windowed_bytes(context))
    }

    /// The largest context `budget` bytes of VRAM can hold.
    ///
    /// `None` when nothing grows — an all-linear or all-windowed checkpoint whose cache
    /// stops expanding, so KV is not what bounds its context. Callers clamp the answer to
    /// the checkpoint's own ceiling; this function does not know it.
    pub fn context_for_budget(&self, budget: u64) -> Option<u64> {
        let window = self.window.unwrap_or(0);
        let win_per_token = self.windowed_layers.saturating_mul(self.row_bytes);

        // Past the window, the sliding layers cost a constant. Solve there first: it is
        // the case that holds for any context large enough to be worth reporting.
        let capped = win_per_token.saturating_mul(window);
        if let Some(left) = budget.checked_sub(capped) {
            if self.bytes_per_token == 0 {
                return None;
            }
            let tokens = left / self.bytes_per_token;
            if tokens >= window {
                return Some(tokens);
            }
        }

        // Inside the window every caching layer is still growing, sliding ones included.
        let per_token = self.bytes_per_token.saturating_add(win_per_token);
        if per_token == 0 {
            return None;
        }
        Some(budget / per_token)
    }

    /// The per-token cost alone, for a one-line summary: `20.0 KiB/tok`.
    pub fn per_token_label(&self) -> String {
        format!("{}/tok", crate::util::bytes(self.bytes_per_token))
    }

    /// The cost and the layer split, for a note: read the second number against the
    /// third, because that is the part nothing else reports.
    pub fn describe(&self) -> String {
        let mut s = format!(
            "{}/token, {} of {} layers cache",
            crate::util::bytes(self.bytes_per_token),
            self.growing_layers,
            self.layers
        );
        let mut rest: Vec<String> = Vec::new();
        if self.windowed_layers > 0 {
            rest.push(match self.window {
                Some(w) => {
                    format!("{} slide within {}", self.windowed_layers, crate::plan::tokens(w))
                }
                None => format!("{} slide", self.windowed_layers),
            });
        }
        if self.flat_layers > 0 {
            rest.push(format!("{} keep a fixed state", self.flat_layers));
        }
        // Commas, not a bracket: this whole phrase is itself parenthesized inside a note,
        // and nesting brackets there reads as a typo.
        for clause in rest {
            s.push_str(", ");
            s.push_str(&clause);
        }
        s
    }
}

/// Read a checkpoint's KV geometry out of its `config.json`.
///
/// `None` when the config does not carry enough to price a row — no layer count, or an
/// attention shape with no head geometry. Saying nothing beats reporting a made-up
/// number against which someone would decide not to download.
pub fn kv_geometry(config: &Value) -> Option<KvGeometry> {
    let text = config.get("text_config");
    // Multimodal wrappers keep the whole language model under `text_config`.
    let field = |key: &str| -> Option<&Value> {
        config
            .get(key)
            .filter(|v| !v.is_null())
            .or_else(|| text.and_then(|t| t.get(key)).filter(|v| !v.is_null()))
    };
    let num = |key: &str| field(key).and_then(Value::as_u64);

    let layers = num("num_hidden_layers").filter(|n| *n > 0)?;

    // A window counts only when the checkpoint says it is using it: plenty of configs
    // carry a `sliding_window` value with `use_sliding_window: false` beside it.
    let window = field("use_sliding_window")
        .and_then(Value::as_bool)
        .unwrap_or(true)
        .then(|| num("sliding_window").or_else(|| num("attention_chunk_size")))
        .flatten()
        .filter(|w| *w > 0);

    let row = match num("kv_lora_rank").filter(|r| *r > 0) {
        // MLA stores one compressed latent plus the un-compressible rotary part.
        Some(rank) => KvRow::Latent { width: rank + num("qk_rope_head_dim").unwrap_or(0) },
        None => {
            let heads = num("num_attention_heads").filter(|n| *n > 0)?;
            let kv_heads = num("num_key_value_heads").filter(|n| *n > 0).unwrap_or(heads);
            let head_dim = num("head_dim")
                .filter(|d| *d > 0)
                .or_else(|| num("hidden_size").map(|h| h / heads))
                .filter(|d| *d > 0)?;
            KvRow::Grouped { kv_heads, head_dim }
        }
    };

    let (growing_layers, windowed_layers, flat_layers) =
        classify_layers(field("layer_types"), layers, window, num("max_window_layers"));
    let row_bytes = row.bytes();

    Some(KvGeometry {
        layers,
        growing_layers,
        windowed_layers,
        flat_layers,
        row,
        row_bytes,
        bytes_per_token: growing_layers.saturating_mul(row_bytes),
        window,
    })
}

/// Split the layers into growing, windowed and flat.
///
/// `layer_types` is the modern spelling and is believed when present. Without it the
/// shape has to come from the older fields: a checkpoint declaring an active
/// `sliding_window` slides on its lowest `max_window_layers` layers and attends fully
/// above them, which is Qwen2's convention and the only one these fields can express.
fn classify_layers(
    types: Option<&Value>,
    layers: u64,
    window: Option<u64>,
    max_window_layers: Option<u64>,
) -> (u64, u64, u64) {
    if let Some(list) = types.and_then(Value::as_array).filter(|l| !l.is_empty()) {
        let (mut growing, mut windowed, mut flat) = (0, 0, 0);
        for entry in list {
            let name = entry.as_str().unwrap_or_default();
            if FLAT_TYPES.contains(&name) {
                flat += 1;
            } else if WINDOWED_TYPES.contains(&name) && window.is_some() {
                windowed += 1;
            } else if GROWING_TYPES.contains(&name) {
                growing += 1;
            } else {
                // Everything unrecognized, and any windowed name with no window to cap
                // it, is priced as the expensive case. A layer kind this does not know
                // is a reason to over-estimate the cache, never to drop it silently.
                growing += 1;
            }
        }
        return (growing, windowed, flat);
    }

    match window {
        Some(_) => {
            let windowed = max_window_layers.unwrap_or(layers).min(layers);
            (layers - windowed, windowed, 0)
        }
        None => (layers, 0, 0),
    }
}

/// What this card can actually hold of a checkpoint's context, and how sure that is.
struct KvBudget {
    /// VRAM left for the cache after the weights that must stay resident.
    budget: u64,
    /// Largest context the budget affords, already clamped to the checkpoint's ceiling.
    /// `None` when KV is not what bounds it.
    servable: Option<u64>,
    /// What else wants this budget but could not be priced from a config.
    unpriced: Unpriced,
}

/// Costs that come out of the same VRAM as the KV cache and that a `config.json` cannot
/// settle. Each one makes `servable` a ceiling rather than a plan, and each has to be
/// named: "at most" with no reason attached is a hedge, not a finding.
#[derive(Debug, Clone, Copy, Default)]
struct Unpriced {
    /// An offloaded MoE keeps an unknowable share of its expert banks in VRAM — how much
    /// is `--moe-cache-*`, a choice the reader has not made yet.
    weights: bool,
    /// Linear-attention layers hold a fixed state per slot, and the pool is sized by
    /// `--max-running-requests` against dimensions no config spells out. `plan.rs` prices
    /// it from a served engine's `/v1/cache/status`; nothing can price it before that.
    state_pool: bool,
}

impl Unpriced {
    fn any(self) -> bool {
        self.weights || self.state_pool
    }

    fn caveat(self) -> &'static str {
        match (self.weights, self.state_pool) {
            (true, true) => ", before any weights, expert cache or state pool",
            (true, false) => ", before any weights or expert cache",
            (false, true) => ", before its linear-attention state pool",
            (false, false) => "",
        }
    }

    /// "at most about" where the figure is a ceiling, "about" where it is the answer.
    fn hedge(self) -> &'static str {
        if self.any() {
            "at most about "
        } else {
            "about "
        }
    }
}

/// Price the cache against the card.
///
/// The subtlety is what counts as resident. A dense model is served resident and its
/// whole download sits in VRAM. A MoE small enough to fit does the same, on the fused
/// backend. A MoE too large for the card runs offloaded, and how much of its expert banks
/// stays in VRAM is a choice the operator has not made yet — so rather than invent a
/// split, this prices the cache against a card holding *no* weights and says that the
/// answer is an upper bound. That is still decisive: a checkpoint that cannot reach a
/// useful context with the weights taken out of the picture certainly cannot with them
/// in it.
fn kv_budget(kv: &KvGeometry, ceiling: Option<u64>, download_bytes: u64, vram: u64) -> KvBudget {
    let usable = (vram as f64 * MEMORY_RATIO) as u64;
    let fits_resident = download_bytes > 0 && download_bytes <= usable;
    let resident = if fits_resident { download_bytes } else { 0 };
    let unpriced = Unpriced { weights: !fits_resident, state_pool: kv.flat_layers > 0 };
    let budget = usable.saturating_sub(resident);
    let servable = kv
        .context_for_budget(budget)
        .map(|tokens| match ceiling {
            // Never advertise past the checkpoint's own ceiling: the card having room
            // for 400k of a 256k model means it has room for 256k.
            Some(c) => tokens.min(c),
            None => tokens,
        })
        .or(ceiling);
    KvBudget { budget, servable, unpriced }
}

/// The note: what this card holds of this checkpoint's context, and at what cost.
///
/// `floor` is the least context worth having, which is the reader's own
/// `--kv-reserve-tokens` where they have set one and the engine's default where they have
/// not. Taking it from there rather than picking a number means the blocker answers *this*
/// reader's requirement: someone serving 256k conversations and someone serving 8k ones
/// are not asking the same question of the same card.
fn kv_note(kv: &KvGeometry, ceiling: Option<u64>, floor: KvFloor, b: &KvBudget) -> (Level, String) {
    use crate::plan::tokens;
    use crate::util::bytes;
    // Said once, wherever the figure is a ceiling rather than a plan, and always with
    // the reason: "at most" on its own tells a reader to distrust the number without
    // telling them what would sharpen it.
    let caveat = b.unpriced.caveat();

    let Some(servable) = b.servable else {
        // Nothing grows and nothing bounds it. Worth saying plainly, because a cache that
        // does not grow with the conversation is the least intuitive good news here.
        return (
            Level::Info,
            format!("its cache does not grow with the conversation — {}", kv.describe()),
        );
    };

    if servable < floor.tokens {
        return (
            Level::Blocker,
            format!(
                "cannot hold {} on this card: {} of KV against {} of VRAM budget{} ({})",
                floor.describe(),
                bytes(kv.bytes_for_context(floor.tokens)),
                bytes(b.budget),
                caveat,
                kv.describe(),
            ),
        );
    }

    let Some(ceiling) = ceiling else {
        return (
            Level::Info,
            format!(
                "this card's KV budget holds {}{}{} ({})",
                b.unpriced.hedge(),
                tokens(servable),
                caveat,
                kv.describe()
            ),
        );
    };

    if servable >= ceiling {
        return (
            Level::Info,
            format!(
                "holds its full {} of context here{} ({})",
                tokens(ceiling),
                caveat,
                kv.describe()
            ),
        );
    }

    // The gap is the finding, so the note names both numbers whatever its severity.
    let level = if (servable as f64) < ceiling as f64 * KV_CAUTION_FRACTION {
        Level::Caution
    } else {
        Level::Info
    };
    (
        level,
        format!(
            "serves {}{} of its advertised {} on this card{} ({})",
            b.unpriced.hedge(),
            tokens(servable),
            tokens(ceiling),
            caveat,
            kv.describe(),
        ),
    )
}

/// The least context worth having, and where that number came from.
#[derive(Debug, Clone, Copy)]
struct KvFloor {
    tokens: u64,
    /// True when the reader set `--kv-reserve-tokens` themselves, which changes the
    /// sentence: a blocker against their own stated requirement reads very differently
    /// from one against a default they have never seen.
    chosen: bool,
}

impl KvFloor {
    fn new(wanted: Option<u64>) -> Self {
        match wanted.filter(|w| *w > 0) {
            Some(tokens) => KvFloor { tokens, chosen: true },
            None => KvFloor { tokens: crate::plan::DEFAULT_KV_RESERVE_TOKENS, chosen: false },
        }
    }

    /// The whole noun phrase, not just the number: "even 8k of context" and "the 256k of
    /// context --kv-reserve-tokens asks for" need different sentences around them, and
    /// splicing a bare figure into one of them strands the other.
    fn describe(self) -> String {
        let t = crate::plan::tokens(self.tokens);
        if self.chosen {
            format!("the {t} of context --kv-reserve-tokens asks for")
        } else {
            format!("even {t} of context")
        }
    }
}

/// Evaluate a repo from its config and the size of the files that would be downloaded.
///
/// `supported_archs` is FreeToken's own registry; `None` means it could not be consulted,
/// which is reported rather than guessed at.
///
/// `wanted_context` is the reader's own `--kv-reserve-tokens`, when they have set one. It
/// is what the KV blocker is judged against, so the verdict answers the context this
/// reader actually serves rather than a floor paddock picked on their behalf.
pub fn evaluate(
    config: &Value,
    download_bytes: u64,
    supported_archs: Option<&[String]>,
    hw: Hardware,
    wanted_context: Option<u64>,
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

    // ---- KV cache: the wall that weights offload past and the cache cannot ----
    //
    // Deliberately outside the `download_bytes > 0` block above. The per-token cost comes
    // from the config alone, so it is knowable the moment the check runs — before the file
    // listing has landed, and before a reader has picked which quantization to pull. The
    // size only sharpens the ceiling; its absence must not silence the finding.
    r.kv = kv_geometry(config);
    if let (Some(kv), true) = (&r.kv, hw.vram_bytes > 0) {
        let ceiling = r.context.filter(|c| *c > 0);
        let budget = kv_budget(kv, ceiling, download_bytes, hw.vram_bytes);
        let (level, msg) = kv_note(kv, ceiling, KvFloor::new(wanted_context), &budget);
        r.max_servable_context = budget.servable;
        r.context_is_upper_bound = budget.unpriced.any();
        r.notes.push((level, msg));
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
        let r = evaluate(&cfg, 20 << 30, Some(&archs()), roomy(), None);
        assert_eq!(r.verdict(), Verdict::Supported, "{:?}", r.notes);
        assert!(r.is_moe);
        assert_eq!(r.num_experts, Some(128));
        assert!(r.summary().contains("MoE x128"));
    }

    #[test]
    fn an_unregistered_architecture_is_a_blocker() {
        let cfg = json!({"architectures": ["SomeNewThingForCausalLM"], "model_type": "x"});
        let r = evaluate(&cfg, 0, Some(&archs()), roomy(), None);
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
        let r = evaluate(&cfg, 23 << 30, Some(&archs()), roomy(), None);
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
        let r = evaluate(&cfg, 20 << 30, Some(&known), roomy(), None);
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
        let r = evaluate(&covered, 20 << 30, Some(&archs()), roomy(), None);
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
        let r = evaluate(&bare, 20 << 30, Some(&archs()), roomy(), None);
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
        let r = evaluate(&cfg, 20 << 30, Some(&archs()), roomy(), None);
        assert_eq!(r.verdict(), Verdict::Supported, "{:?}", r.notes);
    }

    /// An unquantized MoE has no expert quantization to resolve, so nothing to flag.
    #[test]
    fn an_unquantized_moe_is_not_flagged() {
        let cfg = json!({
            "architectures": ["Qwen3MoeForCausalLM"],
            "num_experts": 128
        });
        let r = evaluate(&cfg, 20 << 30, Some(&archs()), roomy(), None);
        assert_eq!(r.verdict(), Verdict::Supported, "{:?}", r.notes);
    }

    #[test]
    fn a_dense_model_larger_than_vram_is_a_blocker_but_an_moe_is_not() {
        let dense = json!({"architectures": ["LlamaForCausalLM"], "num_hidden_layers": 80});
        let r = evaluate(&dense, 40 << 30, Some(&archs()), roomy(), None);
        assert_eq!(r.verdict(), Verdict::Unsupported);
        assert!(r.notes[0].1.contains("served resident"));

        let moe = json!({"architectures": ["Qwen3MoeForCausalLM"], "num_experts": 128});
        let r = evaluate(&moe, 40 << 30, Some(&archs()), roomy(), None);
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
        let r = evaluate(&cfg, 20 << 30, Some(&archs()), tight, None);
        assert_eq!(r.verdict(), Verdict::Caution);
        assert!(r.notes.iter().any(|(_, m)| m.contains("second copy")));

        // Does not fit at all.
        let r = evaluate(&cfg, 60 << 30, Some(&archs()), tight, None);
        assert_eq!(r.verdict(), Verdict::Unsupported);
        assert!(r.notes.iter().any(|(_, m)| m.contains("only")));

        // Bigger than host RAM: the expert banks have nowhere to live.
        let r = evaluate(&cfg, 50 << 30, Some(&archs()), roomy(), None);
        assert!(r.notes.iter().any(|(_, m)| m.contains("host RAM")));
    }

    #[test]
    fn an_unavailable_registry_is_admitted_not_guessed() {
        let cfg = json!({"architectures": ["Whatever"], "num_experts": 4});
        let r = evaluate(&cfg, 0, None, roomy(), None);
        assert_eq!(r.verdict(), Verdict::Caution);
        assert!(r.notes.iter().any(|(_, m)| m.contains("unverified")));
    }

    #[test]
    fn a_config_with_no_architecture_cannot_be_dispatched() {
        let r = evaluate(&json!({"model_type": "mystery"}), 0, Some(&archs()), roomy(), None);
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
            None,
        );
        let levels: Vec<Level> = r.notes.iter().map(|(l, _)| *l).collect();
        let mut sorted = levels.clone();
        sorted.sort_by(|a, b| b.cmp(a));
        assert_eq!(levels, sorted, "the blocker must be the first thing read");
    }

    // ---------------------------------------------------------------- KV geometry

    /// Expand a run-length layer plan into the `layer_types` array a config carries.
    fn layer_types(plan: &[(&str, u64)]) -> Value {
        let mut out = Vec::new();
        for (kind, n) in plan {
            for _ in 0..*n {
                out.push(Value::String((*kind).to_string()));
            }
        }
        Value::Array(out)
    }

    fn kib(n: u64) -> u64 {
        n * 1024
    }

    /// Section 7 of `docs/kv-geometry.md`, as Rust.
    ///
    /// Every row is the live `config.json` of a real repo, reduced to the fields that
    /// price a cache. They are here as a table rather than as one test each because the
    /// point they make is comparative: these eight checkpoints are all within a factor of
    /// two on parameter count and span more than a factor of twelve on cache cost, and no
    /// single row shows that.
    #[test]
    fn the_fixture_checkpoints_price_as_measured() {
        // repo, config, layers, growing, windowed, flat, bytes/token
        let cases: Vec<(&str, Value, u64, u64, u64, u64, u64)> = vec![
            (
                "Qwen/Qwen3.6-35B-A3B",
                json!({"text_config": {
                    "num_hidden_layers": 40, "num_attention_heads": 16,
                    "num_key_value_heads": 2, "head_dim": 256, "hidden_size": 2048,
                    "max_position_embeddings": 262144,
                    "layer_types": layer_types(&[("linear_attention", 30), ("full_attention", 10)]),
                }}),
                40,
                10,
                0,
                30,
                kib(20),
            ),
            (
                // MLA: one compressed latent for the layer, not one row per head. Its
                // `head_dim` is 0 and its `qk_rope_head_dim` absent, both of which the
                // latent path has to step over rather than divide by.
                "RedHatAI/GLM-5.3-Flash-NVFP4",
                json!({"text_config": {
                    "num_hidden_layers": 45, "num_attention_heads": 64,
                    "num_key_value_heads": 64, "head_dim": 0, "hidden_size": 4096,
                    "max_position_embeddings": 1048576, "kv_lora_rank": 512,
                    "qk_rope_head_dim": 0,
                    "layer_types": layer_types(&[
                        ("linear_attention", 34), ("deepseek_sparse_attention", 11),
                    ]),
                }}),
                45,
                11,
                0,
                34,
                kib(11),
            ),
            (
                "meta-models/Muse-Glimmer-30B",
                json!({"text_config": {
                    "num_hidden_layers": 52, "num_attention_heads": 32,
                    "num_key_value_heads": 2, "head_dim": 128, "hidden_size": 6656,
                    "max_position_embeddings": 131072, "sliding_window": 2048,
                    "layer_types": layer_types(&[
                        ("sliding_attention", 39), ("full_attention", 13),
                    ]),
                }}),
                52,
                13,
                39,
                0,
                kib(13),
            ),
            (
                "google/gemma-4-26B-A4B-it",
                json!({"text_config": {
                    "num_hidden_layers": 30, "num_attention_heads": 16,
                    "num_key_value_heads": 8, "head_dim": 256, "hidden_size": 2816,
                    "max_position_embeddings": 262144, "sliding_window": 1024,
                    "layer_types": layer_types(&[
                        ("sliding_attention", 25), ("full_attention", 5),
                    ]),
                }}),
                30,
                5,
                25,
                0,
                kib(40),
            ),
            (
                // Not wrapped in a `text_config`, and interleaved one for one.
                "openai/gpt-oss-120b",
                json!({
                    "num_hidden_layers": 36, "num_attention_heads": 64,
                    "num_key_value_heads": 8, "head_dim": 64, "hidden_size": 2880,
                    "max_position_embeddings": 131072, "sliding_window": 128,
                    "layer_types": layer_types(&[
                        ("sliding_attention", 18), ("full_attention", 18),
                    ]),
                }),
                36,
                18,
                18,
                0,
                kib(36),
            ),
            (
                "Qwen/Qwen3.8-27B",
                json!({"text_config": {
                    "num_hidden_layers": 64, "num_attention_heads": 24,
                    "num_key_value_heads": 4, "head_dim": 256, "hidden_size": 5120,
                    "max_position_embeddings": 262144,
                    "layer_types": layer_types(&[
                        ("linear_attention", 48), ("full_attention", 16),
                    ]),
                }}),
                64,
                16,
                0,
                48,
                kib(64),
            ),
            (
                // The case that motivated the whole check: no `layer_types` at all, and a
                // `sliding_window` its own config switches off, so every one of its 48
                // layers caches and nothing caps any of them.
                "IFM/K2-Horizon-MoVA-36B-A4B",
                json!({
                    "num_hidden_layers": 48, "num_attention_heads": 32,
                    "num_key_value_heads": 8, "head_dim": 128, "hidden_size": 2560,
                    "max_position_embeddings": 524288, "use_sliding_window": false,
                }),
                48,
                48,
                0,
                0,
                kib(192),
            ),
            (
                "nvidia/MiniMax-M2.5-NVFP4",
                json!({
                    "num_hidden_layers": 62, "num_attention_heads": 48,
                    "num_key_value_heads": 8, "head_dim": 128, "hidden_size": 3072,
                    "max_position_embeddings": 196608,
                }),
                62,
                62,
                0,
                0,
                kib(248),
            ),
        ];

        for (repo, cfg, layers, growing, windowed, flat, per_token) in cases {
            let kv = kv_geometry(&cfg).unwrap_or_else(|| panic!("{repo} should price"));
            assert_eq!(kv.layers, layers, "{repo} layers");
            assert_eq!(kv.growing_layers, growing, "{repo} growing");
            assert_eq!(kv.windowed_layers, windowed, "{repo} windowed");
            assert_eq!(kv.flat_layers, flat, "{repo} flat");
            assert_eq!(kv.bytes_per_token, per_token, "{repo} bytes/token");
            assert_eq!(
                kv.growing_layers + kv.windowed_layers + kv.flat_layers,
                layers,
                "{repo}: every layer has to be accounted for somewhere"
            );
        }
    }

    /// The headline of the whole feature: parameter count says nothing about cache cost.
    /// Both of these are 35B-class MoE checkpoints that offload their weights the same
    /// way, and one caches nearly ten times cheaper than the other.
    #[test]
    fn two_checkpoints_of_the_same_class_differ_tenfold_in_cache_cost() {
        let hybrid = kv_geometry(&json!({"text_config": {
            "num_hidden_layers": 40, "num_attention_heads": 16, "num_key_value_heads": 2,
            "head_dim": 256, "hidden_size": 2048,
            "layer_types": layer_types(&[("linear_attention", 30), ("full_attention", 10)]),
        }}))
        .unwrap();
        let full = kv_geometry(&json!({
            "num_hidden_layers": 48, "num_attention_heads": 32, "num_key_value_heads": 8,
            "head_dim": 128, "hidden_size": 2560,
        }))
        .unwrap();

        assert_eq!(hybrid.bytes_per_token, kib(20));
        assert_eq!(full.bytes_per_token, kib(192));
        // 5 GiB against 48 GiB at the same context, on checkpoints of the same size.
        assert_eq!(hybrid.bytes_for_context(262_144), 5 << 30);
        assert_eq!(full.bytes_for_context(262_144), 48 << 30);
    }

    /// A sliding layer costs rows until its window is full and a constant after, so the
    /// total is piecewise and the seam is where an off-by-one would hide.
    #[test]
    fn a_sliding_window_stops_costing_once_it_is_full() {
        let kv = kv_geometry(&json!({
            "num_hidden_layers": 4, "num_attention_heads": 8, "num_key_value_heads": 8,
            "head_dim": 64, "sliding_window": 1024,
            "layer_types": layer_types(&[("sliding_attention", 2), ("full_attention", 2)]),
        }))
        .unwrap();
        // 2 x 8 heads x 64 x 2 bytes = 2 KiB a row.
        assert_eq!(kv.row_bytes, kib(2));
        assert_eq!(kv.bytes_per_token, kib(4), "only the two full layers grow");

        // Inside the window all four layers are still filling.
        assert_eq!(kv.bytes_for_context(512), kib(4) * 512 + kib(4) * 512);
        // At the window exactly, and for ever after, the sliding pair costs a constant.
        let capped = kib(4) * 1024;
        assert_eq!(kv.windowed_bytes(1024), capped);
        assert_eq!(kv.windowed_bytes(1_000_000), capped, "it forgets, it does not grow");
        assert_eq!(kv.bytes_for_context(4096), kib(4) * 4096 + capped);
    }

    /// `context_for_budget` inverts `bytes_for_context`, including across the window seam
    /// where the two halves of the piecewise solution meet.
    #[test]
    fn the_context_a_budget_affords_inverts_what_a_context_costs() {
        let kv = kv_geometry(&json!({
            "num_hidden_layers": 4, "num_attention_heads": 8, "num_key_value_heads": 8,
            "head_dim": 64, "sliding_window": 1024,
            "layer_types": layer_types(&[("sliding_attention", 2), ("full_attention", 2)]),
        }))
        .unwrap();

        for budget in [1 << 20, 8 << 20, kib(4) * 1024 * 2, 64 << 20, 1 << 30] {
            let tokens = kv.context_for_budget(budget).expect("full layers bound it");
            assert!(
                kv.bytes_for_context(tokens) <= budget,
                "{tokens} tokens must fit in {budget} bytes"
            );
            assert!(
                kv.bytes_for_context(tokens + 1) > budget,
                "{tokens} must be the most that fits in {budget} bytes"
            );
        }
    }

    /// An all-linear checkpoint's cache does not grow with the conversation, so no budget
    /// bounds its context and the honest answer is "not this".
    #[test]
    fn a_cache_that_does_not_grow_is_not_bounded_by_the_budget() {
        let kv = kv_geometry(&json!({
            "num_hidden_layers": 8, "num_attention_heads": 8, "num_key_value_heads": 8,
            "head_dim": 64,
            "layer_types": layer_types(&[("linear_attention", 8)]),
        }))
        .unwrap();
        assert_eq!(kv.bytes_per_token, 0);
        assert_eq!(kv.flat_layers, 8);
        assert_eq!(kv.context_for_budget(1 << 30), None);
    }

    /// Without `layer_types`, an active `sliding_window` is read Qwen2's way: the lowest
    /// `max_window_layers` slide, the rest attend fully. Only those top layers grow, and
    /// reading the field the other way round would report the cost of the whole model.
    #[test]
    fn the_older_sliding_window_fields_are_read_qwen2s_way() {
        let base = json!({
            "num_hidden_layers": 32, "num_attention_heads": 8, "num_key_value_heads": 8,
            "head_dim": 64, "sliding_window": 4096,
        });

        let mut partial = base.clone();
        partial["max_window_layers"] = json!(28);
        let kv = kv_geometry(&partial).unwrap();
        assert_eq!((kv.growing_layers, kv.windowed_layers), (4, 28));

        // No `max_window_layers` at all: the window covers the model.
        let kv = kv_geometry(&base).unwrap();
        assert_eq!((kv.growing_layers, kv.windowed_layers), (0, 32));

        // And a window the config says it is not using is not a window.
        let mut off = base.clone();
        off["use_sliding_window"] = json!(false);
        let kv = kv_geometry(&off).unwrap();
        assert_eq!((kv.growing_layers, kv.windowed_layers), (32, 0));
        assert_eq!(kv.window, None);
    }

    /// A layer kind this does not recognize is priced as the expensive case. Guessing low
    /// tells a reader a model fits when it does not, which is the one failure mode here
    /// that costs an hour and a download.
    #[test]
    fn an_unrecognized_layer_kind_is_priced_as_the_expensive_case() {
        let kv = kv_geometry(&json!({
            "num_hidden_layers": 4, "num_attention_heads": 8, "num_key_value_heads": 8,
            "head_dim": 64,
            "layer_types": layer_types(&[("some_new_attention", 2), ("linear_attention", 2)]),
        }))
        .unwrap();
        assert_eq!(kv.growing_layers, 2, "the unknown kind caches until proven otherwise");
        assert_eq!(kv.flat_layers, 2);

        // A sliding layer with no window to cap it is the same situation.
        let kv = kv_geometry(&json!({
            "num_hidden_layers": 2, "num_attention_heads": 8, "num_key_value_heads": 8,
            "head_dim": 64,
            "layer_types": layer_types(&[("sliding_attention", 2)]),
        }))
        .unwrap();
        assert_eq!((kv.growing_layers, kv.windowed_layers), (2, 0));
    }

    /// `head_dim` is optional and `num_key_value_heads` falls back to the query heads, but
    /// a config with no attention shape at all cannot be priced and must say so rather
    /// than report a number someone would decide against a download on.
    #[test]
    fn a_config_without_an_attention_shape_is_not_priced_at_all() {
        // head_dim derived from hidden_size, kv heads defaulted to the query heads: MHA.
        let kv = kv_geometry(&json!({
            "num_hidden_layers": 2, "num_attention_heads": 32, "hidden_size": 4096,
        }))
        .unwrap();
        assert_eq!(kv.row, KvRow::Grouped { kv_heads: 32, head_dim: 128 });
        assert_eq!(kv.bytes_per_token, 2 * (2 * 32 * 128 * 2));

        assert!(kv_geometry(&json!({"num_hidden_layers": 48})).is_none());
        assert!(kv_geometry(&json!({"num_attention_heads": 32, "hidden_size": 4096})).is_none());
        assert!(kv_geometry(&json!({"num_hidden_layers": 0, "num_attention_heads": 8})).is_none());
    }

    /// The motivating case. K2-Horizon's weights offload like any other 36B-A4B MoE and
    /// its architecture is a tractable port, so the day a loader lands upstream the
    /// architecture check stops catching it. What remains is 48 of 48 layers caching at
    /// 192 KiB a token — and unlike architecture support, that number never improves.
    ///
    /// Note what this does *not* claim. On a 16 GiB card the checkpoint still reaches
    /// something like 77k before a single byte of weights is paid for, so "it cannot
    /// serve 8k" would be false. The true finding is the share: a sixth of what the repo
    /// advertises, and that is the upper bound, not the plan.
    #[test]
    fn a_model_whose_weights_offload_is_still_priced_by_its_cache() {
        let cfg = json!({
            "architectures": ["K2HorizonForCausalLM"], "model_type": "k2_horizon",
            "num_experts": 128, "num_hidden_layers": 48, "num_attention_heads": 32,
            "num_key_value_heads": 8, "head_dim": 128, "hidden_size": 2560,
            "max_position_embeddings": 524288, "use_sliding_window": false,
        });
        let known = vec!["K2HorizonForCausalLM".to_string()];
        let card =
            Hardware { vram_bytes: 16 << 30, host_ram_bytes: 64 << 30, free_disk_bytes: 500 << 30 };

        let r = evaluate(&cfg, 20 << 30, Some(&known), card, None);
        assert_eq!(r.kv.as_ref().unwrap().bytes_per_token, kib(192));
        assert_eq!(r.verdict(), Verdict::Caution, "{:?}", r.notes);
        let note = &r.notes[0].1;
        assert!(note.contains("at most"), "it is a ceiling, not a plan: {note}");
        assert!(note.contains("of its advertised 512k"), "name the gap: {note}");
        assert!(note.contains("48 of 48 layers cache"), "and why it is there: {note}");
        // Nothing about the architecture: upstream support is not what bounds this.
        assert!(!r.notes.iter().any(|(_, m)| m.contains("registry")), "{:?}", r.notes);

        // A reader who serves 256k conversations is asking a different question of the
        // same card, and gets a different answer.
        let r = evaluate(&cfg, 20 << 30, Some(&known), card, Some(262_144));
        assert_eq!(r.verdict(), Verdict::Unsupported, "{:?}", r.notes);
        assert!(r.notes[0].1.contains("--kv-reserve-tokens asks for"), "{:?}", r.notes);

        // And on a card with room for it, the same checkpoint reads as supported.
        let big = Hardware { vram_bytes: 180 << 30, ..card };
        let r = evaluate(&cfg, 20 << 30, Some(&known), big, None);
        assert_eq!(r.verdict(), Verdict::Supported, "{:?}", r.notes);
        assert_eq!(r.max_servable_context, Some(524_288), "never advertise past the ceiling");
    }

    /// The finding is the gap, so the note names both numbers at every severity. What
    /// changes with severity is only the glyph: a card serving half of what a repo
    /// advertises is worth telling someone about, but it is not a warning, and a caution
    /// that fires on every modern checkpoint would be one nobody reads.
    #[test]
    fn the_gap_is_always_named_and_only_a_severe_one_warns() {
        let cfg = json!({
            "architectures": ["LlamaForCausalLM"],
            "num_hidden_layers": 48, "num_attention_heads": 32, "num_key_value_heads": 8,
            "head_dim": 128, "hidden_size": 2560, "max_position_embeddings": 262144,
        });
        let known = ["LlamaForCausalLM".to_string()];
        let card = |gib: u64| Hardware {
            vram_bytes: gib << 30,
            host_ram_bytes: 64 << 30,
            free_disk_bytes: 500 << 30,
        };

        // 48 GiB, 20 GiB of it weights: about half the advertised ceiling. Stated, not
        // warned about.
        let half = evaluate(&cfg, 20 << 30, Some(&known), card(48), None);
        assert_eq!(half.verdict(), Verdict::Supported, "{:?}", half.notes);
        let note = half.notes.iter().find(|(_, m)| m.contains("advertised")).expect("a gap note");
        assert_eq!(note.0, Level::Info);
        assert!(note.1.contains("of its advertised 256k"), "{}", note.1);
        assert!(note.1.contains("48 of 48 layers cache"), "{}", note.1);
        assert!(!half.context_is_upper_bound, "a dense model's weights are all resident");
        let servable = half.max_servable_context.unwrap();
        assert!((crate::plan::DEFAULT_KV_RESERVE_TOKENS..262_144).contains(&servable));

        // 24 GiB against the same weights: under a quarter of the ceiling, which is where
        // the gap stops being a detail.
        let sliver = evaluate(&cfg, 20 << 30, Some(&known), card(24), None);
        assert_eq!(sliver.verdict(), Verdict::Caution, "{:?}", sliver.notes);
        assert!(sliver.max_servable_context.unwrap() < servable);
    }

    /// An offloaded MoE keeps an unknown share of its expert banks in VRAM, so the
    /// context figure is a ceiling rather than a plan — and has to read as one.
    #[test]
    fn an_offloaded_moe_reports_a_ceiling_and_says_that_is_what_it_is() {
        let cfg = json!({
            "architectures": ["Qwen3MoeForCausalLM"], "num_experts": 128,
            "num_hidden_layers": 48, "num_attention_heads": 32, "num_key_value_heads": 8,
            "head_dim": 128, "hidden_size": 2560, "max_position_embeddings": 262144,
        });
        let card =
            Hardware { vram_bytes: 48 << 30, host_ram_bytes: 96 << 30, free_disk_bytes: 500 << 30 };

        // 80 GiB of weights on a 48 GiB card: offloaded, so the weights cannot be priced.
        let r = evaluate(&cfg, 80 << 30, Some(&["Qwen3MoeForCausalLM".to_string()]), card, None);
        assert!(r.context_is_upper_bound);
        let note = r.notes.iter().find(|(_, m)| m.contains("advertised")).expect("a gap note");
        assert!(note.1.contains("at most about"), "hedged where it is a ceiling: {}", note.1);

        // The same model small enough to fuse is priced exactly, and lower, because its
        // weights are now competing for the same VRAM.
        let fused =
            evaluate(&cfg, 20 << 30, Some(&["Qwen3MoeForCausalLM".to_string()]), card, None);
        assert!(!fused.context_is_upper_bound);
        assert!(
            fused.max_servable_context < r.max_servable_context,
            "priced weights leave less for the cache: {:?} vs {:?}",
            fused.max_servable_context,
            r.max_servable_context
        );
    }

    /// Two different things come out of the same VRAM and neither can be settled from a
    /// config: an offloaded MoE's resident expert banks, and a linear-attention model's
    /// state pool. Each makes the context figure a ceiling, and the note has to say which
    /// one — "at most" with no reason attached tells a reader to distrust the number
    /// without telling them what would sharpen it.
    #[test]
    fn every_cost_that_cannot_be_priced_is_named_in_the_hedge() {
        let known = ["Qwen3MoeForCausalLM".to_string()];
        let card =
            Hardware { vram_bytes: 48 << 30, host_ram_bytes: 96 << 30, free_disk_bytes: 500 << 30 };
        let attention = json!({
            "num_attention_heads": 32, "num_key_value_heads": 8, "head_dim": 128,
            "max_position_embeddings": 262144,
        });
        let build = |layers: Value, moe: bool| {
            let mut cfg = attention.clone();
            cfg["architectures"] = json!(["Qwen3MoeForCausalLM"]);
            cfg["num_hidden_layers"] = json!(48);
            if moe {
                cfg["num_experts"] = json!(128);
            }
            cfg["layer_types"] = layers;
            cfg
        };
        let all_full = layer_types(&[("full_attention", 48)]);
        let hybrid = layer_types(&[("linear_attention", 36), ("full_attention", 12)]);

        // Weights that fit, no linear layers: nothing unpriced, so nothing hedged.
        let r = evaluate(&build(all_full.clone(), false), 20 << 30, Some(&known), card, None);
        assert!(!r.context_is_upper_bound);
        assert!(!r.notes.iter().any(|(_, m)| m.contains("at most")), "{:?}", r.notes);

        // Weights too big to be resident.
        let r = evaluate(&build(all_full, true), 80 << 30, Some(&known), card, None);
        assert!(r.context_is_upper_bound);
        assert!(
            r.notes.iter().any(|(_, m)| m.contains("before any weights or expert cache")),
            "{:?}",
            r.notes
        );

        // Weights that fit, but 36 layers holding a state pool nothing here can size.
        let r = evaluate(&build(hybrid.clone(), false), 20 << 30, Some(&known), card, None);
        assert!(r.context_is_upper_bound);
        assert!(
            r.notes.iter().any(|(_, m)| m.contains("before its linear-attention state pool")),
            "{:?}",
            r.notes
        );

        // Both at once, named together rather than twice.
        let r = evaluate(&build(hybrid, true), 80 << 30, Some(&known), card, None);
        assert!(
            r.notes
                .iter()
                .any(|(_, m)| m.contains("before any weights, expert cache or state pool")),
            "{:?}",
            r.notes
        );
    }

    /// The per-token cost is the one field that makes two candidate repos comparable at a
    /// glance, so it belongs on the line a reader compares them on.
    #[test]
    fn the_summary_line_carries_the_per_token_cost() {
        let cfg = json!({
            "architectures": ["Qwen3MoeForCausalLM"], "num_experts": 128,
            "text_config": {
                "num_hidden_layers": 40, "num_attention_heads": 16, "num_key_value_heads": 2,
                "head_dim": 256, "hidden_size": 2048, "max_position_embeddings": 262144,
                "layer_types": layer_types(&[("linear_attention", 30), ("full_attention", 10)]),
            }
        });
        let r = evaluate(&cfg, 20 << 30, Some(&archs()), roomy(), None);
        let summary = r.summary();
        assert!(summary.contains("256k ctx"), "{summary}");
        assert!(summary.contains("20.0 KiB/tok"), "{summary}");
    }

    /// The cache is priced from the config alone, so it is knowable before the file
    /// listing lands and before a reader has picked a quantization. The size only sharpens
    /// the ceiling; its absence must not silence the finding.
    #[test]
    fn the_cache_is_priced_even_with_no_download_size_yet() {
        let cfg = json!({
            "architectures": ["LlamaForCausalLM"],
            "num_hidden_layers": 48, "num_attention_heads": 32, "num_key_value_heads": 8,
            "head_dim": 128, "max_position_embeddings": 262144,
        });
        let r = evaluate(&cfg, 0, Some(&["LlamaForCausalLM".to_string()]), roomy(), None);
        assert_eq!(r.kv.as_ref().unwrap().bytes_per_token, kib(192));
        assert!(r.notes.iter().any(|(_, m)| m.contains("192 KiB/token")), "{:?}", r.notes);
        assert!(r.context_is_upper_bound, "nothing is known about the weights yet");
    }

    /// Without a card to price against there is no context claim to make. Saying "8k"
    /// because VRAM read as zero would be worse than saying nothing.
    #[test]
    fn no_hardware_means_no_context_claim() {
        let cfg = json!({
            "architectures": ["LlamaForCausalLM"],
            "num_hidden_layers": 48, "num_attention_heads": 32, "num_key_value_heads": 8,
            "head_dim": 128, "max_position_embeddings": 262144,
        });
        let r =
            evaluate(&cfg, 0, Some(&["LlamaForCausalLM".to_string()]), Hardware::default(), None);
        assert!(r.kv.is_some(), "the geometry is a property of the checkpoint");
        assert_eq!(r.max_servable_context, None);
        assert_eq!(r.verdict(), Verdict::Supported, "{:?}", r.notes);
    }

    /// `MEMORY_RATIO` restates `--memory-ratio`'s default. If the knob moves and this does
    /// not, every context figure here is quietly wrong by the difference.
    #[test]
    fn memory_ratio_matches_the_knob() {
        let knob = crate::knobs::knob("memory_ratio").expect("the knob exists");
        assert_eq!(
            knob.default.parse::<f64>().expect("a numeric default"),
            MEMORY_RATIO,
            "--memory-ratio's default moved; MEMORY_RATIO has to move with it"
        );
    }

    /// The floor a blocker is judged against is the reader's own `--kv-reserve-tokens`
    /// where they set one, and the engine's default where they did not. Nothing here is a
    /// number paddock picked, which is what makes the blocker defensible.
    #[test]
    fn the_blocker_floor_is_the_readers_own_or_the_engines() {
        assert_eq!(KvFloor::new(None).tokens, crate::plan::DEFAULT_KV_RESERVE_TOKENS);
        assert!(!KvFloor::new(None).chosen);
        assert_eq!(KvFloor::new(Some(262_144)).tokens, 262_144);
        assert!(KvFloor::new(Some(262_144)).chosen);
        // A zero is "not set", not "no context wanted".
        assert_eq!(KvFloor::new(Some(0)).tokens, crate::plan::DEFAULT_KV_RESERVE_TOKENS);
    }
}
