//! Deciding how to serve a model on this machine, and saying why.
//!
//! FreeToken resolves nearly everything itself. The two places it deliberately does not
//! are the two that decide whether a serve is fast and whether it can use the context the
//! checkpoint advertises:
//!
//! **The cache split is MoE-first, and the loser is silent.** With `--moe-cache-auto` —
//! on by default for every offload-family backend — `plan_cache_budget` reserves
//! `--kv-reserve-tokens` (default 8192) for KV, lets the expert cache greedily take the
//! rest, and gives KV whatever survives. The engine then sets
//! `max_seq_len = min(model_ceiling, num_pages * page_size)` with no warning, while
//! `/v1/models` keeps advertising the model ceiling on purpose ("a rebuild moves the
//! latter"). So a 256k checkpoint can serve 8k and nothing anywhere says so. The lever
//! back is `--kv-reserve-tokens`: set it to the context you actually want and the
//! MoE-first split reserves that first.
//!
//! **`--moe-backend auto` never picks `fused`**, because the engine cannot know whether
//! the experts would fit in HBM and a wrong guess is a weight-load OOM rather than a
//! slower-but-working run. ft-man does know — it has NVML and the model's own geometry —
//! so it is the right place to offer the choice the engine will not make.
//!
//! Everything here is integer arithmetic over quantities the engine has already measured
//! and published, so it is unit-testable without a GPU. Nothing in this module estimates
//! a number it could instead be told: a plan is built only from a [`Costs`] the engine
//! reported, and its absence is reported rather than guessed around.

use serde::{Deserialize, Serialize};

use crate::ft::types::{BenchProfile, CacheGeometry};
use crate::knobs::ServeConfig;

/// FreeToken's own default for `--kv-reserve-tokens`: the KV floor the MoE-first split
/// holds back before experts take the rest.
pub const DEFAULT_KV_RESERVE_TOKENS: u64 = 8192;

// ---------------------------------------------------------------- costs

/// The per-unit VRAM costs a plan is priced against, exactly as the engine reported them.
///
/// Every field here comes from `GET /v1/cache/status`; none of it is derived from a
/// checkpoint on disk. That is deliberate. The per-token KV cost depends on the attention
/// group layout, the cache dtype, the TP shard and any sliding-window split, and the
/// per-expert cost depends on the packed quantization format — reconstructing either from
/// `config.json` means reimplementing FreeToken's own cost model and being quietly wrong
/// when it changes. Asking the engine once and remembering the answer is both exact and
/// stable across upgrades.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Costs {
    /// The engine's net VRAM budget for the MoE and KV pools together, after weights and
    /// the fixed (non-paged) cache, before the `(1 - memory_ratio)` graph headroom.
    pub cache_budget_bytes: u64,
    pub kv_bytes_per_token: u64,
    pub moe_bytes_per_expert: u64,
    /// KV page granularity. Usable context is `num_pages * page_size`.
    pub page_size: u64,
    /// Per-layer expert count — the floor unit for the slot cache.
    pub experts_per_layer: u64,
    /// Every expert slot the model has, the ceiling for the MoE cache.
    pub total_experts: u64,
    /// Bytes one GDN (linear-attention) state slot occupies. Zero for a model without a
    /// linear group. These are big — tens of MiB each — so the pool is not a rounding
    /// error: on a hybrid-linear MoE it can be a seventh of the whole cache budget.
    pub mamba_bytes_per_slot: u64,
    /// PHYSICAL state slots, which is one more than the geometry reports: the padding
    /// sink is allocated but is not a usable slot, and it still costs VRAM.
    pub mamba_slots: u64,
}

impl Costs {
    /// Read the costs off a live engine's geometry.
    ///
    /// `None` when the engine has not published its `unit_bytes` ack yet — a server that
    /// is still loading answers `/v1/cache/status` with zeros, and pricing a plan against
    /// a zero divides by it.
    pub fn from_geometry(geo: &CacheGeometry) -> Option<Self> {
        let u = &geo.unit_bytes;
        if u.kv_per_token == 0 || geo.cache_budget_bytes == 0 {
            return None;
        }
        Some(Self {
            cache_budget_bytes: geo.cache_budget_bytes,
            kv_bytes_per_token: u.kv_per_token,
            moe_bytes_per_expert: u.moe_per_expert,
            page_size: geo.page_size.max(1),
            experts_per_layer: geo.num_experts,
            total_experts: geo.total_experts(),
            mamba_bytes_per_slot: u.mamba_per_slot,
            // The geometry reports usable slots; the pool also allocates a padding sink.
            mamba_slots: if geo.num_mamba_slots > 0 { geo.num_mamba_slots + 1 } else { 0 },
        })
    }

    /// True when the model has an expert cache competing with KV for the budget. A dense
    /// model hands the whole budget to KV and none of the MoE trade-off applies.
    pub fn is_moe(&self) -> bool {
        self.moe_bytes_per_expert > 0 && self.total_experts > 0
    }

    /// True when the model carries a GDN/linear-attention state pool.
    pub fn is_hybrid_linear(&self) -> bool {
        self.mamba_bytes_per_slot > 0 && self.mamba_slots > 0
    }

    /// VRAM the GDN state pool occupies.
    pub fn mamba_bytes(&self) -> u64 {
        self.mamba_slots.saturating_mul(self.mamba_bytes_per_slot)
    }

    /// What is actually left for the MoE and KV pools.
    ///
    /// `cache_budget_bytes` is the engine's total for *every* paged pool, and on a
    /// hybrid-linear model the GDN state pool is one of them — the engine subtracts it
    /// (`available_memory -= state_pool_bytes(config)`) before solving for KV. Pricing
    /// context against the gross budget overstates it by the whole state pool, which on a
    /// 24-slot pool at 61 MiB a slot is over a gigabyte of context that does not exist.
    pub fn net_budget(&self) -> u64 {
        self.cache_budget_bytes.saturating_sub(self.mamba_bytes())
    }

    /// The same costs with the state pool resized, as a different
    /// `--max-running-requests` would size it.
    pub fn with_mamba_slots(&self, slots: u64) -> Self {
        Self { mamba_slots: slots, ..*self }
    }

    /// Context, in tokens, that the budget affords once `slots` experts are resident.
    /// Saturates at zero rather than wrapping when the slots alone overrun the budget.
    pub fn context_for_slots(&self, slots: u64) -> u64 {
        let spent = slots.saturating_mul(self.moe_bytes_per_expert);
        let left = self.net_budget().saturating_sub(spent);
        let tokens = left / self.kv_bytes_per_token;
        // Context is only usable a whole page at a time.
        tokens / self.page_size * self.page_size
    }

    /// The most experts that can stay resident while still leaving room for `tokens` of
    /// context, clamped to what the model actually has.
    ///
    /// `None` when the budget cannot hold that much context at any slot count — the
    /// caller reports the ceiling instead of pretending a plan exists.
    pub fn slots_for_context(&self, tokens: u64) -> Option<u64> {
        let kv_bytes = tokens.checked_mul(self.kv_bytes_per_token)?;
        let left = self.net_budget().checked_sub(kv_bytes)?;
        if !self.is_moe() {
            return Some(0);
        }
        Some((left / self.moe_bytes_per_expert).min(self.total_experts))
    }

    /// The smallest expert cache the engine will build.
    ///
    /// One full layer of experts, doubled when prefill overlap is on — the overlap
    /// borrows two whole expert-layer buffers, so it raises the floor and therefore
    /// lowers the context that can be reached. This is why `--disable-moe-prefill-overlap`
    /// is sometimes what buys the last of the context.
    pub fn floor_slots(&self, prefill_overlap: bool) -> u64 {
        if !self.is_moe() {
            return 0;
        }
        let floor =
            if prefill_overlap { 2 * self.experts_per_layer } else { self.experts_per_layer };
        floor.min(self.total_experts)
    }

    /// The largest context this budget can serve, which is what is left once the expert
    /// cache is squeezed to its floor.
    pub fn max_context(&self, prefill_overlap: bool) -> u64 {
        self.context_for_slots(self.floor_slots(prefill_overlap))
    }
}

// ---------------------------------------------------------------- context fit

/// What the engine will actually serve against what the checkpoint claims.
///
/// The whole point of this type is that the two numbers are different and nothing else
/// puts them side by side: `usable` is `num_pages * page_size` (engine.py's own
/// `min(max_seq_len, num_tokens)` input) and `ceiling` is what `/v1/models` reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextFit {
    pub usable: u64,
    pub ceiling: u64,
}

impl ContextFit {
    /// Compare a live geometry against the served model's advertised context.
    ///
    /// `None` when either number is missing — a server still loading has published
    /// neither, and "0 of 0 tokens" is a worse thing to show than nothing.
    pub fn measure(geo: &CacheGeometry, ceiling: u64) -> Option<Self> {
        let usable = geo.num_pages.checked_mul(geo.page_size.max(1))?;
        (usable > 0 && ceiling > 0).then_some(Self { usable, ceiling })
    }

    /// True when the engine will refuse prompts the model's own card says it accepts.
    pub fn is_truncated(&self) -> bool {
        self.usable < self.ceiling
    }

    /// How much of the advertised context is really there, 0.0 to 1.0.
    pub fn ratio(&self) -> f64 {
        crate::util::ratio(self.usable.min(self.ceiling), self.ceiling)
    }

    /// A short phrase for a status line: `"32k of 256k"`.
    pub fn summary(&self) -> String {
        format!("{} of {}", tokens(self.usable), tokens(self.ceiling))
    }
}

/// Token counts read better in k/M than in full digits, and every context number in the
/// UI is one of a small set of round values.
pub fn tokens(n: u64) -> String {
    match n {
        0 => "0".into(),
        n if n >= 1 << 20 && n % (1 << 20) == 0 => format!("{}M", n >> 20),
        n if n >= 1 << 20 => format!("{:.1}M", n as f64 / (1u64 << 20) as f64),
        n if n >= 1024 && n % 1024 == 0 => format!("{}k", n >> 10),
        n if n >= 1024 => format!("{:.1}k", n as f64 / 1024.0),
        n => n.to_string(),
    }
}

// ---------------------------------------------------------------- startup budget

/// The geometry `--moe-cache-auto` will settle on at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Startup {
    pub moe_cache_size: u64,
    pub num_pages: u64,
    pub prefill_overlap: bool,
}

impl Startup {
    pub fn usable_tokens(&self, page_size: u64) -> u64 {
        self.num_pages * page_size.max(1)
    }
}

/// Predict the MoE-first split for a given `--kv-reserve-tokens`.
///
/// A faithful port of FreeToken's `cache_budget.plan_cache_budget`, kept in step with it
/// deliberately: this is what lets ft-man say what a configuration *will* do before the
/// engine spends four minutes loading weights to demonstrate it. The Python raises on the
/// two impossible cases; here they are `Err`, because a planner that panics is worse than
/// one that says the budget is too small.
pub fn plan_cache_budget(
    costs: &Costs,
    kv_reserve_tokens: u64,
    prefill_overlap: bool,
) -> Result<Startup, String> {
    if costs.kv_bytes_per_token == 0 {
        return Err("the engine has not published a KV cost yet".into());
    }
    let page_size = costs.page_size.max(1);
    let cache_per_page = costs.kv_bytes_per_token * page_size;
    let kv_reserve_pages = kv_reserve_tokens.div_ceil(page_size);
    let budget = costs.net_budget();

    if !costs.is_moe() {
        // A dense model has no expert cache; the whole budget is KV.
        let num_pages = budget / cache_per_page;
        if num_pages <= 1 {
            return Err("not enough VRAM for a KV cache".into());
        }
        return Ok(Startup { moe_cache_size: 0, num_pages, prefill_overlap: false });
    }

    let hi = costs.total_experts;
    // Prefill overlap borrows two full expert-layer buffers, so it needs >= 2x the
    // per-layer expert count; below that the engine turns it off rather than fail.
    let mut overlap = prefill_overlap && hi >= 2 * costs.experts_per_layer;
    let lo = if overlap { 2 * costs.experts_per_layer } else { costs.experts_per_layer };

    let kv_reserve_bytes = kv_reserve_pages.saturating_mul(cache_per_page);
    let raw = budget.saturating_sub(kv_reserve_bytes) / costs.moe_bytes_per_expert;
    let moe_cache_size = raw.clamp(lo.min(hi), hi);
    overlap = overlap && moe_cache_size >= 2 * costs.experts_per_layer;

    let remaining = budget.saturating_sub(moe_cache_size * costs.moe_bytes_per_expert);
    let num_pages = (remaining / cache_per_page).max(kv_reserve_pages);

    // The floor can push the plan past the budget when VRAM is genuinely too tight; the
    // engine asserts here, so refuse in arithmetic rather than let it OOM at load.
    let total = moe_cache_size * costs.moe_bytes_per_expert + num_pages * cache_per_page;
    if total > budget {
        return Err(format!(
            "the smallest possible plan ({} expert slots + {} KV tokens) needs {}, \
             over the engine's {} cache budget",
            moe_cache_size,
            tokens(num_pages * page_size),
            crate::util::bytes(total),
            crate::util::bytes(budget),
        ));
    }
    if num_pages <= 1 {
        return Err("not enough VRAM for a KV cache after the expert cache".into());
    }
    Ok(Startup { moe_cache_size, num_pages, prefill_overlap: overlap })
}

// ---------------------------------------------------------------- GDN state pool

/// FreeToken's own default for `linear_state_cache_ratio`. Not a CLI flag — it is a
/// `ServerArgs` field with no argparse entry — so a plan can read it but never set it.
pub const LINEAR_STATE_CACHE_RATIO: f64 = 2.0;

/// FreeToken's own default for `--max-running-requests`.
pub const DEFAULT_MAX_RUNNING: u64 = 4;

/// Free host RAM below which the CPU-side backends stop being safe to recommend.
/// The banks themselves are already pinned by then; this is the room everything
/// *else* needs — the CPU executor's buffers, the page cache, the tokenizer procs.
pub const HOST_RAM_HEADROOM: u64 = 4 << 30;

/// Physical GDN state slots the engine allocates for `max_running` concurrent requests.
///
/// A port of `linear_state_pool._linear_pool_num_slots`. The hybrid-radix pool holds four
/// slots per running request (one live, two ping-pong, one committed snapshot locked
/// through decode), plus a cross-request snapshot cache and a padding sink; the naive
/// path keeps the older `max_running + 1`. The engine picks the hybrid-radix variant
/// automatically for any GDN model unless `--cache-type naive` was asked for.
///
/// This is the whole reason `--max-running-requests` is a memory knob and not just a
/// scheduling one: at 61 MiB a slot, going from one request to four costs a gigabyte.
pub fn mamba_slots_for(max_running: u64, hybrid_radix: bool, cache_ratio: f64) -> u64 {
    let mr = max_running.max(1);
    if !hybrid_radix {
        return mr + 1;
    }
    let n_cache = ((cache_ratio * mr as f64) as u64).max(4);
    4 * mr + n_cache + 1
}

// ---------------------------------------------------------------- the plan

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Worth knowing; nothing to change.
    Info,
    /// A knob that should be set differently.
    Advice,
    /// Something will not work as configured.
    Warning,
}

/// One recommendation: a knob to set, or a fact worth stating.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub level: Level,
    /// The knob to change, and what to change it to. `None` for a note that carries no
    /// edit — a missing bench profile is a job to run, not a flag to set.
    pub set: Option<(&'static str, String)>,
    /// Why. Shown verbatim, so it says what it knows and how it knows it.
    pub reason: String,
}

impl Step {
    fn note(level: Level, reason: impl Into<String>) -> Self {
        Self { level, set: None, reason: reason.into() }
    }

    fn set(
        level: Level,
        key: &'static str,
        value: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self { level, set: Some((key, value.into())), reason: reason.into() }
    }

    /// How the change reads in a list: `--kv-reserve-tokens 262144`.
    pub fn label(&self) -> String {
        match &self.set {
            Some((key, value)) => {
                let flag = crate::knobs::knob(key).map(|k| k.flag).unwrap_or(key);
                if value == "true" {
                    flag.to_string()
                } else {
                    format!("{flag} {value}")
                }
            }
            None => "—".into(),
        }
    }
}

/// What a planning run concluded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub steps: Vec<Step>,
    /// The context the plan expects to deliver, and the model's ceiling.
    pub fit: Option<ContextFit>,
    /// Set when there was not enough measured information to plan the memory split.
    pub unpriced: Option<String>,
}

impl Plan {
    /// The edits, in schema order, ready to fold into a [`ServeConfig`].
    pub fn edits(&self) -> Vec<(&'static str, String)> {
        self.steps.iter().filter_map(|s| s.set.clone()).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Apply every edit. Returns how many knobs actually changed, so the UI can say
    /// "already optimal" rather than claim it did something.
    pub fn apply(&self, serve: &mut ServeConfig) -> usize {
        let mut changed = 0;
        for (key, value) in self.edits() {
            if serve.get(key) != Some(value.as_str()) {
                serve.set(key, value);
                changed += 1;
            }
        }
        changed
    }

    fn push(&mut self, step: Step) {
        self.steps.push(step);
    }
}

/// What the planner knows about the machine it is planning for.
#[derive(Debug, Clone, Default)]
pub struct Machine {
    pub gpu_name: Option<String>,
    /// Host RAM, which the offload backends spend as freely as VRAM: every expert lives
    /// in pinned host banks, and the CPU MoE executor wants working room on top.
    pub host_ram_total: u64,
    pub host_ram_available: u64,
    /// Physical cores, which is what the CPU MoE executor wants — not SMT threads.
    pub physical_cores: usize,
    pub pcie_link: Option<String>,
}

/// The model being planned for.
#[derive(Debug, Clone, Default)]
pub struct Target {
    pub name: String,
    /// `max_position_embeddings`: the context the checkpoint claims.
    pub ceiling: Option<u64>,
    pub is_moe: bool,
    /// The expert format, as the checkpoint declares it — the key the bench profile joins on.
    pub quant: Option<String>,
}

/// Build a plan for `target` on `machine`.
///
/// `costs` is what the engine measured for this model, from a live serve or a remembered
/// one; without it the memory half of the plan is skipped and said to be skipped, because
/// the alternative is inventing a KV cost and confidently sizing a cache against it.
pub fn build(
    target: &Target,
    machine: &Machine,
    costs: Option<&Costs>,
    bench: Option<&BenchProfile>,
    serve: &ServeConfig,
) -> Plan {
    let mut plan = Plan::default();

    plan_context(&mut plan, target, costs, serve);
    plan_concurrency(&mut plan, target, costs, serve);
    plan_backend(&mut plan, target, machine, costs, bench, serve);
    plan_host_memory(&mut plan, machine, costs);
    plan_speed(&mut plan, machine, serve);

    plan
}

/// `--max-running-requests` on a hybrid-linear model, where it is a memory knob.
///
/// Two facts collide here. The GDN state pool costs four slots per running request at
/// tens of MiB each, out of the same budget the expert cache draws on. And the KV pool
/// holds a fixed number of tokens, so at full context it may only have room for *one*
/// request anyway. When both are true, concurrency is being paid for and cannot be
/// delivered, and handing those slots to the expert cache is close to free.
fn plan_concurrency(plan: &mut Plan, target: &Target, costs: Option<&Costs>, serve: &ServeConfig) {
    let Some(costs) = costs.filter(|c| c.is_hybrid_linear() && c.is_moe()) else { return };
    let Some(fit) = plan.fit else { return };
    let _ = target;

    let current = serve
        .get("max_running_requests")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_MAX_RUNNING);
    if current <= 1 {
        return;
    }
    let hybrid_radix = serve.get("cache_type") != Some("naive");

    // Only advise when the ported formula reproduces what the engine actually allocated.
    // If it does not, this build sizes the pool differently and a recommendation derived
    // from the formula would be arithmetic about nothing.
    let predicted = mamba_slots_for(current, hybrid_radix, LINEAR_STATE_CACHE_RATIO);
    if predicted != costs.mamba_slots {
        plan.push(Step::note(
            Level::Info,
            format!(
                "the GDN state pool holds {} slots ({}); this build sizes it differently than \
                 expected for --max-running-requests {current}, so it is left alone",
                costs.mamba_slots,
                crate::util::bytes(costs.mamba_bytes()),
            ),
        ));
        return;
    }

    // The trade only pays when the expert cache is capped by the budget. If every expert
    // is already resident, freeing state slots buys nothing and cutting concurrency would
    // be a downgrade paid for no gain.
    let now = costs.slots_for_context(fit.usable).unwrap_or(0);
    if now >= costs.total_experts {
        return;
    }

    let target_slots = mamba_slots_for(1, hybrid_radix, LINEAR_STATE_CACHE_RATIO);
    let freed = costs.mamba_slots.saturating_sub(target_slots) * costs.mamba_bytes_per_slot;
    let after = costs.with_mamba_slots(target_slots).slots_for_context(fit.usable).unwrap_or(now);
    if after <= now {
        return;
    }

    // KV is pinned at its reserve, so the MoE-first split spends the freed bytes on
    // experts — which is the point: a bigger resident cache is fewer PCIe fetches.
    plan.push(Step::set(
        Level::Advice,
        "max_running_requests",
        "1".to_string(),
        format!(
            "the GDN state pool costs {} at {current} running requests, four slots each at \
             {}. KV holds {}, which is one request at the {} this plan reserves, so most of \
             that pool cannot be used at this context. Dropping to 1 frees {} and grows the \
             expert cache from {} to {} slots of {} ({:.0}% resident), which is fewer PCIe \
             fetches per decode step. The cost is real: {current} shorter requests could \
             otherwise run at once, so keep {current} if you serve more than one caller.",
            crate::util::bytes(costs.mamba_bytes()),
            crate::util::bytes(costs.mamba_bytes_per_slot),
            tokens(fit.usable),
            tokens(fit.ceiling),
            crate::util::bytes(freed),
            now,
            after,
            costs.total_experts,
            crate::util::ratio(after, costs.total_experts) * 100.0,
        ),
    ));
}

/// Host RAM, which the offload family spends as freely as VRAM.
///
/// Every expert lives in pinned host banks whether or not it is resident on the GPU, so
/// the host-side footprint is the *whole* model's experts, not the cache. On a box where
/// that is most of RAM, hybrid and `--expert-load parallel` stop being free choices.
fn plan_host_memory(plan: &mut Plan, machine: &Machine, costs: Option<&Costs>) {
    let Some(costs) = costs.filter(|c| c.is_moe()) else { return };
    if machine.host_ram_total == 0 {
        return;
    }
    let banks = costs.total_experts.saturating_mul(costs.moe_bytes_per_expert);
    let share = crate::util::ratio(banks, machine.host_ram_total);

    if machine.host_ram_available < HOST_RAM_HEADROOM {
        plan.push(Step::note(
            Level::Warning,
            format!(
                "host RAM is nearly spoken for: {} free of {}, with about {} of it \
                 pinned expert banks. That rules out --moe-backend hybrid (its CPU \
                 executor needs working room) and makes --expert-load parallel risky, \
                 since it buffers a whole shard.",
                crate::util::bytes(machine.host_ram_available),
                crate::util::bytes(machine.host_ram_total),
                crate::util::bytes(banks),
            ),
        ));
    } else if share > 0.5 {
        plan.push(Step::note(
            Level::Info,
            format!(
                "the pinned expert banks are about {} of this host's {} of RAM ({:.0}%);                  keep --expert-load on auto rather than parallel, which buffers a whole shard",
                crate::util::bytes(banks),
                crate::util::bytes(machine.host_ram_total),
                share * 100.0,
            ),
        ));
    }
}

/// The context half: spend the budget on KV first, up to the model's ceiling.
fn plan_context(plan: &mut Plan, target: &Target, costs: Option<&Costs>, serve: &ServeConfig) {
    let Some(ceiling) = target.ceiling.filter(|c| *c > 0) else {
        plan.unpriced = Some("the checkpoint does not declare a context length".into());
        return;
    };
    let Some(costs) = costs else {
        plan.unpriced = Some(
            "no VRAM costs have been measured for this model yet, so the cache split \
             cannot be planned; serve it once and the plan will be exact"
                .into(),
        );
        return;
    };

    // Prefill overlap doubles the expert floor, so it is sometimes the reason context is
    // short. Giving it up costs prefill-copy speed; running out of context is a wall, so
    // the plan trades the first for the second whenever the trade actually buys context
    // that is wanted — not only when it reaches the whole ceiling.
    let overlap = !serve.flag("disable_moe_prefill_overlap");
    let mut reachable = costs.max_context(overlap);
    if overlap && reachable < ceiling && costs.max_context(false) > reachable {
        let freed = costs.max_context(false);
        plan.push(Step::set(
            Level::Advice,
            "disable_moe_prefill_overlap",
            "true",
            format!(
                "the two-buffer prefill overlap pins {} expert slots that context could be \
                 using. Giving it up slows prefill copies and raises the reachable context \
                 from {} to {}.",
                costs.floor_slots(true) - costs.floor_slots(false),
                tokens(reachable),
                tokens(freed.min(ceiling)),
            ),
        ));
        reachable = freed;
    }

    let want = ceiling.min(reachable);
    plan.fit = Some(ContextFit { usable: want, ceiling });

    // A budget that cannot seat the expert floor and a usable KV pool at once is not a
    // context problem, it is a "this model does not fit on this card" problem, and no
    // amount of --kv-reserve-tokens fixes it.
    if want == 0 {
        plan.push(Step::note(
            Level::Warning,
            format!(
                "{} does not fit on this GPU: the {}-slot expert floor alone spends the \
                 engine's whole {} cache budget, leaving nothing for KV. A smaller \
                 checkpoint, or a card with more VRAM, is the only way through.",
                target.name,
                costs.floor_slots(false),
                crate::util::bytes(costs.net_budget()),
            ),
        ));
        return;
    }

    if reachable < ceiling {
        plan.push(Step::note(
            Level::Warning,
            format!(
                "this GPU cannot hold {} of context for {}: with the expert cache squeezed \
                 to its {}-slot floor the budget affords {}. Everything below plans for that.",
                tokens(ceiling),
                target.name,
                costs.floor_slots(overlap && reachable == costs.max_context(true)),
                tokens(reachable),
            ),
        ));
    }

    // --kv-reserve-tokens is the lever: the MoE-first split reserves it for KV before
    // experts take the rest, so setting it to the wanted context buys exactly that.
    let current = serve
        .get("kv_reserve_tokens")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_KV_RESERVE_TOKENS);
    if current >= want {
        plan.push(Step::note(
            Level::Info,
            format!(
                "--kv-reserve-tokens is already {}, which reserves the {} this plan wants",
                tokens(current),
                tokens(want)
            ),
        ));
    } else {
        let slots = costs.slots_for_context(want).unwrap_or(0);
        // What today's reserve actually yields, under the overlap setting in force now —
        // that is the number the user can check against the running engine.
        let default_ctx = plan_cache_budget(costs, current, overlap)
            .map(|s| s.usable_tokens(costs.page_size))
            .unwrap_or(0);
        plan.push(Step::set(
            Level::Advice,
            "kv_reserve_tokens",
            want.to_string(),
            if costs.is_moe() {
                format!(
                    "reserve {} {} of context before the expert cache takes the rest. \
                     At the current {} the split leaves KV about {}; the experts give up \
                     {} slots of {} to pay for it.",
                    if want == ceiling { "the full" } else { "the reachable" },
                    tokens(want),
                    tokens(current),
                    tokens(default_ctx),
                    costs.total_experts.saturating_sub(slots),
                    costs.total_experts,
                )
            } else {
                format!("hold {} of KV rather than the {} default", tokens(want), tokens(current))
            },
        ));
    }
}

/// The backend half: the choice `auto` will not make for itself.
fn plan_backend(
    plan: &mut Plan,
    target: &Target,
    machine: &Machine,
    costs: Option<&Costs>,
    bench: Option<&BenchProfile>,
    serve: &ServeConfig,
) {
    if !target.is_moe {
        return;
    }

    // `fused` keeps every expert resident. auto refuses to pick it because a wrong guess
    // is an OOM at weight load; with a measured per-expert cost the guess is arithmetic.
    if let Some(costs) = costs.filter(|c| c.is_moe()) {
        let all_experts = costs.total_experts * costs.moe_bytes_per_expert;
        let ceiling = target.ceiling.unwrap_or(0);
        let with_full_ctx = all_experts + ceiling * costs.kv_bytes_per_token;
        if ceiling > 0 && with_full_ctx <= costs.net_budget() {
            plan.push(Step::set(
                Level::Advice,
                "moe_backend",
                "fused",
                format!(
                    "every expert fits in VRAM ({}) alongside {} of KV, inside the engine's \
                     {} budget. Resident experts skip the PCIe stream entirely; auto will \
                     never pick this because it cannot prove the fit.",
                    crate::util::bytes(all_experts),
                    tokens(ceiling),
                    crate::util::bytes(costs.net_budget()),
                ),
            ));
            return;
        }
    }

    // Otherwise it is the offload family, and the profile decides offload vs hybrid.
    let explicit = serve.get("moe_backend").filter(|v| *v != "auto");
    let Some(bench) = bench else {
        plan.push(Step::note(
            Level::Warning,
            format!(
                "no `ft bench bw` profile for {}, so --moe-backend auto can only ever pick \
                 offload and --moe-hybrid-max-fetch auto falls back to a fixed cap of 1. \
                 Run a bandwidth benchmark from the Jobs tab{}.",
                machine.gpu_name.as_deref().unwrap_or("this GPU"),
                match &machine.pcie_link {
                    Some(link) => format!(
                        " — on a {link} link with {} physical cores, hybrid is worth measuring",
                        machine.physical_cores
                    ),
                    None => String::new(),
                }
            ),
        ));
        return;
    };

    let format = bench_format(target.quant.as_deref());
    match bench.dtypes.get(&format).and_then(|v| v.clone()) {
        Some(verdict) => {
            let detail = format!(
                "the benchmark on {} recommends {verdict} for {format} experts",
                bench.gpu.name.as_deref().unwrap_or("this GPU"),
            );
            // A benched hybrid verdict is about bandwidth, and bandwidth is not the only
            // thing hybrid spends: its CPU executor wants host RAM that the pinned expert
            // banks may already have taken. Recommending it into a host with no headroom
            // trades a working serve for a faster one that cannot allocate.
            let starved = verdict == "hybrid"
                && machine.host_ram_total > 0
                && machine.host_ram_available < HOST_RAM_HEADROOM;
            if starved {
                plan.push(Step::note(
                    Level::Warning,
                    format!(
                        "{detail}, but only {} of host RAM is free — hybrid's CPU executor \
                         needs working room on top of the pinned banks, so this stays on \
                         offload until there is more headroom",
                        crate::util::bytes(machine.host_ram_available),
                    ),
                ));
            } else if explicit == Some(verdict.as_str()) {
                plan.push(Step::note(
                    Level::Info,
                    format!("--moe-backend is already {verdict}; {detail}"),
                ));
            } else {
                plan.push(Step::set(Level::Advice, "moe_backend", verdict.clone(), detail));
            }
        }
        None => plan.push(Step::note(
            Level::Info,
            format!(
                "the bench profile has no verdict for {format} experts; \
                 `ft bench bw --dtype {format}` would settle the offload/hybrid choice"
            ),
        )),
    }
}

/// The knobs that cost nothing to get right and are never chosen for you.
fn plan_speed(plan: &mut Plan, machine: &Machine, serve: &ServeConfig) {
    // CUDA graphs are captured up to --cuda-graph-max-bs, which defaults to
    // --max-running-requests. Raising concurrency without it leaves the larger batches
    // running eager.
    let running = serve.get("max_running_requests").and_then(|v| v.parse::<u64>().ok());
    if let Some(running) = running {
        let captured = serve.get("cuda_graph_max_bs").and_then(|v| v.parse::<u64>().ok());
        if captured.is_none_or(|c| c < running) {
            plan.push(Step::set(
                Level::Advice,
                "cuda_graph_max_bs",
                running.to_string(),
                format!(
                    "capture CUDA graphs up to the {running} requests the scheduler will \
                     actually run; batches above the captured size decode eager"
                ),
            ));
        }
    }

    // The CPU MoE executor wants one worker per physical core. Its own default (0) already
    // means that, so this is only worth saying when the knob is set to something else.
    if let Some(threads) = serve.get("moe_cpu_threads").and_then(|v| v.parse::<usize>().ok()) {
        if machine.physical_cores > 0 && threads > machine.physical_cores {
            plan.push(Step::set(
                Level::Advice,
                "moe_cpu_threads",
                "0",
                format!(
                    "--moe-cpu-threads {threads} oversubscribes {} physical cores; 0 means \
                     one worker per core, which is what the executor wants",
                    machine.physical_cores
                ),
            ));
        }
    }
}

/// FreeToken's `expert_quant` -> benchbw format key, mirroring `bench_profile.py`'s
/// `_QUANT_TO_BENCH_FORMAT`. An unmapped format falls through unchanged, which is what
/// the Python does, and then simply finds no entry.
fn bench_format(quant: Option<&str>) -> String {
    let q = quant.unwrap_or("bf16").to_lowercase();
    match q.as_str() {
        "mxfp4" => "mxfp4_triton".into(),
        "none" | "" => "bf16".into(),
        other => other.into(),
    }
}

// ---------------------------------------------------------------- bench profile

/// The `ft bench bw` profile written for a GPU, if there is one.
///
/// The lookup mirrors FreeToken's own (`moe/bench_profile.py`): the per-GPU file first,
/// because bandwidth differs between slots, then the newest of whatever else was benched,
/// then the legacy single-file path. Returning the path rather than the parsed profile
/// lets `--doctor` name the file it found without also having to understand it.
pub fn bench_profile_status(gpu_uuid: Option<&str>) -> Option<std::path::PathBuf> {
    let cache = dirs::cache_dir()?.join("freetoken");
    gpu_uuid
        .filter(|u| !u.is_empty())
        .map(|u| cache.join("benchbw").join(format!("{u}.json")))
        .filter(|p| p.is_file())
        .or_else(|| newest_profile(&cache.join("benchbw")))
        .or_else(|| {
            let legacy = cache.join("benchbw.json");
            legacy.is_file().then_some(legacy)
        })
}

/// The most recently written profile in `dir`, whichever GPU it was measured on.
fn newest_profile(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter_map(|p| Some((std::fs::metadata(&p).ok()?.modified().ok()?, p)))
        .max_by_key(|(t, _)| *t)
        .map(|(_, p)| p)
}

// ---------------------------------------------------------------- remembering costs

/// What the engine measured, remembered across runs, keyed by served model name.
///
/// The per-unit costs only become knowable once a model has actually been loaded, which
/// is exactly too late to plan its launch. Writing them down the first time turns that
/// into a one-serve cost: the second launch of a model is planned exactly, and so is
/// every later one, without asking the user to serve a model twice to find out it was
/// serving 8k of a 256k context.
///
/// Keyed by the served model name rather than the checkpoint path, because that is what
/// both `/v1/stats` and a launch configuration agree on.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CostStore {
    #[serde(default)]
    models: std::collections::BTreeMap<String, Costs>,
}

impl CostStore {
    pub fn path() -> std::path::PathBuf {
        crate::config::state_dir().join("costs.json")
    }

    /// Read the store, or an empty one. A corrupt or unreadable file is not worth an
    /// error: the costs are a cache, and the worst case is planning without them.
    pub fn load() -> Self {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    pub fn get(&self, model: &str) -> Option<&Costs> {
        self.models.get(model)
    }

    /// Record what an engine reported. Returns true when this is new information, so the
    /// caller only writes to disk when there is something to write.
    pub fn observe(&mut self, model: &str, costs: Costs) -> bool {
        if self.models.get(model) == Some(&costs) {
            return false;
        }
        self.models.insert(model.to_string(), costs);
        true
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A MoE that comfortably outgrows the card: 40 GiB of experts, 8 GiB of budget.
    fn moe_costs() -> Costs {
        Costs {
            cache_budget_bytes: 8 << 30,
            kv_bytes_per_token: 16 * 1024,
            moe_bytes_per_expert: 8 << 20,
            page_size: 1,
            experts_per_layer: 128,
            total_experts: 128 * 48,
            mamba_bytes_per_slot: 0,
            mamba_slots: 0,
        }
    }

    /// The real geometry off a served Ornith-1.5-35B-A3B-NVFP4 on a 16 GiB RTX 5070 Ti,
    /// taken from its `/v1/cache/status`. Worth carrying verbatim: the three pools sum to
    /// within a kilobyte of the reported budget, so it pins the whole cost model at once.
    fn measured_hybrid_linear() -> Costs {
        Costs {
            cache_budget_bytes: 10_215_371_571,
            kv_bytes_per_token: 20_480,
            moe_bytes_per_expert: 1_775_616,
            page_size: 1,
            experts_per_layer: 256,
            total_experts: 256 * 40,
            mamba_bytes_per_slot: 64_389_120,
            mamba_slots: 25, // 24 reported + the padding sink
        }
    }

    #[test]
    fn context_and_slots_are_inverses_of_each_other() {
        let c = moe_costs();
        let slots = c.slots_for_context(32768).unwrap();
        // Holding that many slots leaves at least the context asked for.
        assert!(c.context_for_slots(slots) >= 32768);
        // One more slot would not.
        assert!(c.context_for_slots(slots + 1) < 32768 + 16);
    }

    #[test]
    fn a_budget_too_small_for_the_context_has_no_slot_count() {
        let c = moe_costs();
        assert_eq!(c.slots_for_context(1 << 30), None, "a petabyte of KV fits nowhere");
    }

    #[test]
    fn max_context_squeezes_the_experts_to_their_floor() {
        let c = moe_costs();
        assert_eq!(c.max_context(false), c.context_for_slots(c.experts_per_layer));
        assert!(c.max_context(false) > c.context_for_slots(c.total_experts));
    }

    #[test]
    fn prefill_overlap_doubles_the_floor_and_so_costs_context() {
        let c = moe_costs();
        assert_eq!(c.floor_slots(true), 2 * c.floor_slots(false));
        assert!(
            c.max_context(true) < c.max_context(false),
            "the overlap's second buffer is context that cannot be spent on KV"
        );
    }

    #[test]
    fn a_floor_never_exceeds_the_experts_the_model_has() {
        // One layer only: doubling it would ask for slots that do not exist.
        let c = Costs { total_experts: 128, experts_per_layer: 128, ..moe_costs() };
        assert_eq!(c.floor_slots(true), 128);
    }

    #[test]
    fn a_dense_model_gives_the_whole_budget_to_kv() {
        let c = Costs { moe_bytes_per_expert: 0, total_experts: 0, ..moe_costs() };
        assert!(!c.is_moe());
        assert_eq!(c.slots_for_context(1024), Some(0));
        assert_eq!(c.floor_slots(true), 0, "a dense model has no expert floor");
        assert_eq!(c.max_context(true), (8u64 << 30) / (16 * 1024));
    }

    // ---- the ported budget split ----

    #[test]
    fn the_default_reserve_starves_kv_and_a_raised_one_does_not() {
        let c = moe_costs();
        let starved = plan_cache_budget(&c, DEFAULT_KV_RESERVE_TOKENS, true).unwrap();
        // Experts take everything they can; KV lands on the reserve floor.
        assert_eq!(starved.usable_tokens(c.page_size), DEFAULT_KV_RESERVE_TOKENS);

        let roomy = plan_cache_budget(&c, 262_144, true).unwrap();
        assert_eq!(roomy.usable_tokens(c.page_size), 262_144);
        assert!(roomy.moe_cache_size < starved.moe_cache_size, "context is paid for in slots");
    }

    #[test]
    fn the_expert_cache_never_goes_below_its_floor() {
        let c = moe_costs();
        // Ask for every token the floor leaves room for; the floor still stands.
        let plan = plan_cache_budget(&c, c.max_context(true), true).unwrap();
        assert_eq!(plan.moe_cache_size, c.floor_slots(true));
        assert!(plan.prefill_overlap);
    }

    #[test]
    fn asking_past_the_overlap_floor_is_refused_rather_than_silently_shrunk() {
        let c = moe_costs();
        // The context that only fits once the overlap gives its second buffer back.
        let beyond = c.max_context(false);
        assert!(plan_cache_budget(&c, beyond, true).is_err(), "does not fit with overlap on");
        assert!(plan_cache_budget(&c, beyond, false).is_ok(), "and fits with it off");
    }

    #[test]
    fn prefill_overlap_turns_itself_off_when_the_slots_are_not_there() {
        // Only one layer's worth of experts can ever be resident.
        let c = Costs { total_experts: 128, ..moe_costs() };
        let plan = plan_cache_budget(&c, 4096, true).unwrap();
        assert!(!plan.prefill_overlap, "overlap needs 2x the per-layer expert count");
    }

    #[test]
    fn a_budget_that_cannot_fit_the_floor_is_an_error_not_a_panic() {
        let c = Costs { cache_budget_bytes: 1 << 20, ..moe_costs() };
        assert!(plan_cache_budget(&c, DEFAULT_KV_RESERVE_TOKENS, true).is_err());
    }

    #[test]
    fn an_unpriced_engine_cannot_be_planned() {
        let c = Costs { kv_bytes_per_token: 0, ..moe_costs() };
        assert!(plan_cache_budget(&c, 4096, true).is_err());
    }

    #[test]
    fn pages_are_whole_units_of_context() {
        let c = Costs { page_size: 128, ..moe_costs() };
        assert_eq!(c.context_for_slots(c.total_experts) % 128, 0);
    }

    // ---- context fit ----

    #[test]
    fn a_truncated_context_is_reported_as_truncated() {
        let fit = ContextFit { usable: 8192, ceiling: 262_144 };
        assert!(fit.is_truncated());
        assert_eq!(fit.summary(), "8k of 256k");
        assert!(fit.ratio() < 0.04);
    }

    #[test]
    fn a_full_context_is_not_truncated() {
        let fit = ContextFit { usable: 262_144, ceiling: 262_144 };
        assert!(!fit.is_truncated());
        assert_eq!(fit.ratio(), 1.0);
    }

    #[test]
    fn a_kv_pool_larger_than_the_model_ceiling_is_still_not_truncated() {
        // num_pages can exceed the ceiling; the engine clamps and nothing is wrong.
        let fit = ContextFit { usable: 300_000, ceiling: 262_144 };
        assert!(!fit.is_truncated());
        assert_eq!(fit.ratio(), 1.0, "the ratio never exceeds one");
    }

    #[test]
    fn token_counts_read_in_round_units() {
        assert_eq!(tokens(0), "0");
        assert_eq!(tokens(512), "512");
        assert_eq!(tokens(8192), "8k");
        assert_eq!(tokens(262_144), "256k");
        assert_eq!(tokens(1 << 20), "1M");
        assert_eq!(tokens(1536), "1.5k");
    }

    // ---- the GDN state pool ----

    #[test]
    fn the_measured_pools_sum_to_the_engines_reported_budget() {
        // The strongest check available without a GPU: if the cost model is right, the
        // three pools the engine actually allocated must add up to what it said its
        // budget was. Taken from a live serve, so any drift in the model shows up here.
        let c = measured_hybrid_linear();
        let kv = 262_230u64 * c.kv_bytes_per_token;
        let moe = 1_822u64 * c.moe_bytes_per_expert;
        let total = kv + moe + c.mamba_bytes();
        let slack = c.cache_budget_bytes - total;
        assert!(slack < 4096, "pools should account for the budget, {slack} B unexplained");
    }

    #[test]
    fn the_state_pool_is_priced_out_of_the_budget_before_context() {
        let c = measured_hybrid_linear();
        assert_eq!(c.net_budget(), c.cache_budget_bytes - c.mamba_bytes());
        // Ignoring the pool would promise about 78k tokens of context that do not exist.
        let overstated = c.cache_budget_bytes / c.kv_bytes_per_token;
        let honest = c.net_budget() / c.kv_bytes_per_token;
        assert!(overstated - honest > 70_000, "the state pool is worth real context");
    }

    #[test]
    fn a_model_without_a_linear_group_has_no_state_pool_to_price() {
        let c = moe_costs();
        assert!(!c.is_hybrid_linear());
        assert_eq!(c.mamba_bytes(), 0);
        assert_eq!(c.net_budget(), c.cache_budget_bytes);
    }

    #[test]
    fn the_slot_formula_reproduces_what_the_engine_allocated() {
        // max_running_requests=4 with the default cache ratio produced 24 usable slots
        // (25 physical) on the real serve.
        assert_eq!(mamba_slots_for(4, true, LINEAR_STATE_CACHE_RATIO), 25);
        assert_eq!(mamba_slots_for(2, true, LINEAR_STATE_CACHE_RATIO), 13);
        assert_eq!(mamba_slots_for(1, true, LINEAR_STATE_CACHE_RATIO), 9);
        // The snapshot cache never drops below four however low concurrency goes.
        assert_eq!(mamba_slots_for(1, true, 0.1), 9);
        // The naive path keeps the older one-per-request sizing.
        assert_eq!(mamba_slots_for(4, false, LINEAR_STATE_CACHE_RATIO), 5);
    }

    #[test]
    fn concurrency_is_traded_for_experts_only_when_kv_cannot_deliver_it() {
        let c = measured_hybrid_linear();
        let t = Target {
            name: "Ornith-1.5-35B-A3B".into(),
            ceiling: Some(262_144),
            is_moe: true,
            quant: Some("nvfp4".into()),
        };
        let plan = build(&t, &machine(), Some(&c), None, &ServeConfig::new());

        // KV holds one full-length request, so four is being paid for and not delivered.
        let step = plan
            .edits()
            .into_iter()
            .find(|(k, _)| *k == "max_running_requests")
            .expect("the state pool should be recommended down");
        assert_eq!(step.1, "1");
        assert!(plan.steps.iter().any(|s| s.reason.contains("fewer PCIe fetches")));
    }

    #[test]
    fn dropping_concurrency_actually_grows_the_expert_cache() {
        let c = measured_hybrid_linear();
        let at_four = c.slots_for_context(262_144).unwrap();
        let at_one = c
            .with_mamba_slots(mamba_slots_for(1, true, LINEAR_STATE_CACHE_RATIO))
            .slots_for_context(262_144)
            .unwrap();
        assert!(at_one > at_four, "freed state slots become expert slots");
        // The freed bytes buy roughly what the arithmetic says they should.
        let freed = (25 - 9) * c.mamba_bytes_per_slot;
        let expected = freed / c.moe_bytes_per_expert;
        // Within one slot: the two divisions carry different remainders.
        assert!((at_one - at_four).abs_diff(expected) <= 1, "{at_one} - {at_four} vs {expected}");
    }

    #[test]
    fn concurrency_is_left_alone_when_the_experts_already_all_fit() {
        // Roomy enough to hold every expert: freeing state slots buys nothing, so cutting
        // concurrency would be a downgrade paid for no gain.
        let c = Costs { cache_budget_bytes: 40 << 30, ..measured_hybrid_linear() };
        let t = Target { ceiling: Some(65_536), is_moe: true, ..Default::default() };
        assert_eq!(c.slots_for_context(65_536), Some(c.total_experts));
        let plan = build(&t, &machine(), Some(&c), None, &ServeConfig::new());
        assert!(!plan.edits().iter().any(|(k, _)| *k == "max_running_requests"));
    }

    #[test]
    fn concurrency_already_at_one_is_left_alone() {
        let mut serve = ServeConfig::new();
        serve.set("max_running_requests", "1");
        let t = Target { ceiling: Some(262_144), is_moe: true, ..Default::default() };
        let plan = build(&t, &machine(), Some(&measured_hybrid_linear()), None, &serve);
        assert!(!plan.edits().iter().any(|(k, _)| *k == "max_running_requests"));
    }

    #[test]
    fn a_pool_the_formula_cannot_reproduce_is_reported_not_resized() {
        // A build that sizes the state pool some other way must not be second-guessed.
        let c = Costs { mamba_slots: 77, ..measured_hybrid_linear() };
        let t = Target { ceiling: Some(262_144), is_moe: true, ..Default::default() };
        let plan = build(&t, &machine(), Some(&c), None, &ServeConfig::new());
        assert!(!plan.edits().iter().any(|(k, _)| *k == "max_running_requests"));
        assert!(plan.steps.iter().any(|s| s.reason.contains("sizes it differently")));
    }

    // ---- host memory ----

    #[test]
    fn a_host_with_no_room_left_is_warned_about() {
        let plan = build(
            &target(),
            &cramped_machine(),
            Some(&measured_hybrid_linear()),
            None,
            &ServeConfig::new(),
        );
        let warn = plan
            .steps
            .iter()
            .find(|s| s.reason.contains("host RAM is nearly spoken for"))
            .expect("thin host RAM should be called out");
        assert_eq!(warn.level, Level::Warning);
        assert!(warn.reason.contains("hybrid"), "it should say what this rules out");
        assert!(warn.set.is_none(), "there is no flag that makes RAM appear");
    }

    #[test]
    fn a_benched_hybrid_verdict_yields_to_a_host_with_no_headroom() {
        let mut bench = BenchProfile::default();
        bench.dtypes.insert("nvfp4".into(), Some("hybrid".into()));
        let plan = build(
            &target(),
            &cramped_machine(),
            Some(&measured_hybrid_linear()),
            Some(&bench),
            &ServeConfig::new(),
        );
        assert!(
            !plan.edits().iter().any(|(k, v)| *k == "moe_backend" && v == "hybrid"),
            "hybrid must not be recommended into a host that cannot feed it"
        );
        assert!(plan.steps.iter().any(|s| s.reason.contains("stays on offload")));
    }

    #[test]
    fn a_benched_hybrid_verdict_stands_when_the_host_has_room() {
        let mut bench = BenchProfile::default();
        bench.dtypes.insert("nvfp4".into(), Some("hybrid".into()));
        let plan = build(
            &target(),
            &machine(),
            Some(&measured_hybrid_linear()),
            Some(&bench),
            &ServeConfig::new(),
        );
        assert!(plan.edits().iter().any(|(k, v)| *k == "moe_backend" && v == "hybrid"));
    }

    #[test]
    fn a_roomy_host_is_not_warned_about() {
        let plan = build(
            &target(),
            &machine(),
            Some(&measured_hybrid_linear()),
            None,
            &ServeConfig::new(),
        );
        assert!(!plan.steps.iter().any(|s| s.reason.contains("host RAM is nearly")));
    }

    #[test]
    fn banks_that_dominate_host_ram_are_noted_even_with_headroom() {
        let m = Machine { host_ram_total: 32 << 30, host_ram_available: 12 << 30, ..machine() };
        let plan = build(&target(), &m, Some(&measured_hybrid_linear()), None, &ServeConfig::new());
        assert!(plan.steps.iter().any(|s| s.reason.contains("expert-load")));
    }

    #[test]
    fn a_host_of_unknown_size_produces_no_memory_advice() {
        let m = Machine { host_ram_total: 0, host_ram_available: 0, ..machine() };
        let plan = build(&target(), &m, Some(&measured_hybrid_linear()), None, &ServeConfig::new());
        assert!(!plan.steps.iter().any(|s| s.reason.contains("host RAM")));
    }

    // ---- the plan ----

    fn target() -> Target {
        Target {
            name: "Qwen3.6-35B".into(),
            ceiling: Some(262_144),
            is_moe: true,
            quant: Some("nvfp4".into()),
        }
    }

    fn machine() -> Machine {
        Machine {
            gpu_name: Some("NVIDIA GeForce RTX 5090".into()),
            host_ram_total: 128 << 30,
            host_ram_available: 96 << 30,
            physical_cores: 24,
            pcie_link: Some("gen5 x16".into()),
        }
    }

    /// The server this was all diagnosed on: 40 GiB of RAM with ~2 GiB left after the
    /// pinned expert banks.
    fn cramped_machine() -> Machine {
        Machine {
            host_ram_total: 40 << 30,
            host_ram_available: 2 << 30,
            physical_cores: 8,
            ..machine()
        }
    }

    #[test]
    fn the_plan_raises_the_kv_reserve_to_the_models_ceiling() {
        // A budget roomy enough to hold the full context.
        let costs = Costs { cache_budget_bytes: 24 << 30, ..moe_costs() };
        let plan = build(&target(), &machine(), Some(&costs), None, &ServeConfig::new());
        let edits = plan.edits();
        assert_eq!(
            edits.iter().find(|(k, _)| *k == "kv_reserve_tokens").map(|(_, v)| v.as_str()),
            Some("262144"),
        );
        assert_eq!(plan.fit.unwrap().usable, 262_144);
        assert!(!plan.fit.unwrap().is_truncated());
    }

    #[test]
    fn a_card_that_cannot_reach_the_ceiling_says_so_and_plans_for_what_it_can() {
        // 2 GiB of budget: even at the expert floor there is nowhere near 256k of KV.
        let costs = Costs { cache_budget_bytes: 2 << 30, ..moe_costs() };
        let plan = build(&target(), &machine(), Some(&costs), None, &ServeConfig::new());
        let fit = plan.fit.unwrap();
        assert!(fit.is_truncated(), "the ceiling is out of reach");
        assert_eq!(fit.usable, costs.max_context(false));
        assert!(plan
            .steps
            .iter()
            .any(|s| s.level == Level::Warning && s.reason.contains("cannot hold")));
        // It still plans for the best reachable context rather than giving up.
        assert!(plan.edits().iter().any(|(k, _)| *k == "kv_reserve_tokens"));
    }

    #[test]
    fn the_prefill_overlap_is_given_up_when_that_is_what_buys_the_full_context() {
        // Sized so the ceiling fits at the one-layer floor but not at the doubled one.
        let c = moe_costs();
        let need = 262_144 * c.kv_bytes_per_token;
        let costs = Costs {
            cache_budget_bytes: need + 3 * c.experts_per_layer / 2 * c.moe_bytes_per_expert,
            ..c
        };
        assert!(costs.max_context(true) < 262_144 && costs.max_context(false) >= 262_144);

        let plan = build(&target(), &machine(), Some(&costs), None, &ServeConfig::new());
        assert_eq!(
            plan.edits()
                .iter()
                .find(|(k, _)| *k == "disable_moe_prefill_overlap")
                .map(|(_, v)| v.as_str()),
            Some("true"),
        );
        assert!(!plan.fit.unwrap().is_truncated(), "and the full context is then reached");
    }

    #[test]
    fn the_overlap_is_kept_when_the_context_already_fits_around_it() {
        // Roomy enough to reach the ceiling with the overlap on: prefill speed is free
        // here, so the plan must not spend it.
        let costs = Costs { cache_budget_bytes: 24 << 30, ..moe_costs() };
        assert!(costs.max_context(true) >= 262_144);
        let plan = build(&target(), &machine(), Some(&costs), None, &ServeConfig::new());
        assert!(!plan.edits().iter().any(|(k, _)| *k == "disable_moe_prefill_overlap"));
    }

    #[test]
    fn a_model_that_cannot_seat_its_expert_floor_is_called_unservable() {
        // The floor alone eats the entire budget: no KV pool is possible at any setting.
        let costs = Costs { cache_budget_bytes: 1 << 30, ..moe_costs() };
        let plan = build(&target(), &machine(), Some(&costs), None, &ServeConfig::new());
        assert_eq!(plan.fit.unwrap().usable, 0);
        assert!(plan
            .steps
            .iter()
            .any(|s| s.level == Level::Warning && s.reason.contains("does not fit on this GPU")));
        assert!(
            !plan.edits().iter().any(|(k, _)| *k == "kv_reserve_tokens"),
            "there is no reserve that rescues a budget this small"
        );
    }

    #[test]
    fn without_measured_costs_the_memory_half_is_skipped_not_guessed() {
        let plan = build(&target(), &machine(), None, None, &ServeConfig::new());
        assert!(plan.unpriced.is_some());
        assert!(plan.fit.is_none());
        assert!(
            !plan.edits().iter().any(|(k, _)| *k == "kv_reserve_tokens"),
            "a cache split must never be invented"
        );
    }

    #[test]
    fn a_missing_bench_profile_is_reported_as_the_speed_it_costs() {
        let plan = build(&target(), &machine(), Some(&moe_costs()), None, &ServeConfig::new());
        let note = plan
            .steps
            .iter()
            .find(|s| s.reason.contains("ft bench bw"))
            .expect("the missing profile should be called out");
        assert_eq!(note.level, Level::Warning);
        assert!(note.set.is_none(), "running a benchmark is a job, not a flag");
        assert!(note.reason.contains("gen5 x16"), "the link it would be measured against");
    }

    #[test]
    fn a_bench_verdict_becomes_the_backend_recommendation() {
        let mut bench = BenchProfile::default();
        bench.gpu.name = Some("NVIDIA GeForce RTX 5090".into());
        bench.dtypes.insert("nvfp4".into(), Some("hybrid".into()));

        let plan =
            build(&target(), &machine(), Some(&moe_costs()), Some(&bench), &ServeConfig::new());
        assert_eq!(
            plan.edits().iter().find(|(k, _)| *k == "moe_backend").map(|(_, v)| v.as_str()),
            Some("hybrid"),
        );
    }

    #[test]
    fn a_backend_already_matching_the_verdict_is_not_re_recommended() {
        let mut bench = BenchProfile::default();
        bench.dtypes.insert("nvfp4".into(), Some("hybrid".into()));
        let mut serve = ServeConfig::new();
        serve.set("moe_backend", "hybrid");

        let plan = build(&target(), &machine(), Some(&moe_costs()), Some(&bench), &serve);
        assert!(!plan.edits().iter().any(|(k, _)| *k == "moe_backend"));
        assert!(plan.steps.iter().any(|s| s.reason.contains("already hybrid")));
    }

    #[test]
    fn experts_that_all_fit_in_vram_earn_the_fused_backend_auto_refuses_to_pick() {
        // A small MoE on a big card: every expert resident, and room for full context.
        let costs = Costs {
            cache_budget_bytes: 24 << 30,
            total_experts: 512,
            experts_per_layer: 64,
            moe_bytes_per_expert: 4 << 20, // 2 GiB of experts in total
            ..moe_costs()
        };
        let plan = build(&target(), &machine(), Some(&costs), None, &ServeConfig::new());
        assert_eq!(
            plan.edits().iter().find(|(k, _)| *k == "moe_backend").map(|(_, v)| v.as_str()),
            Some("fused"),
        );
    }

    #[test]
    fn a_dense_model_gets_no_backend_advice_at_all() {
        let t = Target { is_moe: false, ..target() };
        let costs = Costs { moe_bytes_per_expert: 0, total_experts: 0, ..moe_costs() };
        let plan = build(&t, &machine(), Some(&costs), None, &ServeConfig::new());
        assert!(!plan.edits().iter().any(|(k, _)| *k == "moe_backend"));
        assert!(!plan.steps.iter().any(|s| s.reason.contains("bench bw")));
    }

    #[test]
    fn raising_concurrency_pulls_the_cuda_graph_capture_up_with_it() {
        let mut serve = ServeConfig::new();
        serve.set("max_running_requests", "16");
        let plan = build(&target(), &machine(), Some(&moe_costs()), None, &serve);
        assert_eq!(
            plan.edits().iter().find(|(k, _)| *k == "cuda_graph_max_bs").map(|(_, v)| v.as_str()),
            Some("16"),
        );
    }

    #[test]
    fn a_capture_size_already_covering_concurrency_is_left_alone() {
        let mut serve = ServeConfig::new();
        serve.set("max_running_requests", "4");
        serve.set("cuda_graph_max_bs", "8");
        let plan = build(&target(), &machine(), Some(&moe_costs()), None, &serve);
        assert!(!plan.edits().iter().any(|(k, _)| *k == "cuda_graph_max_bs"));
    }

    #[test]
    fn oversubscribed_cpu_moe_threads_are_pulled_back_to_one_per_core() {
        let mut serve = ServeConfig::new();
        serve.set("moe_cpu_threads", "64");
        let plan = build(&target(), &machine(), Some(&moe_costs()), None, &serve);
        assert_eq!(
            plan.edits().iter().find(|(k, _)| *k == "moe_cpu_threads").map(|(_, v)| v.as_str()),
            Some("0"),
        );
    }

    #[test]
    fn applying_a_plan_reports_only_what_actually_changed() {
        let costs = Costs { cache_budget_bytes: 24 << 30, ..moe_costs() };
        let plan = build(&target(), &machine(), Some(&costs), None, &ServeConfig::new());
        let mut serve = ServeConfig::new();
        let first = plan.apply(&mut serve);
        assert!(first > 0);
        assert_eq!(plan.apply(&mut serve), 0, "re-applying the same plan changes nothing");
        assert_eq!(serve.get("kv_reserve_tokens"), Some("262144"));
    }

    #[test]
    fn a_step_reads_as_the_flag_it_would_set() {
        let s = Step::set(Level::Advice, "kv_reserve_tokens", "262144", "because");
        assert_eq!(s.label(), "--kv-reserve-tokens 262144");
        let flag = Step::set(Level::Advice, "moe_cache_auto", "true", "because");
        assert_eq!(flag.label(), "--moe-cache-auto", "a switch is a bare flag");
    }

    #[test]
    fn bench_format_keys_match_freetokens_own_mapping() {
        assert_eq!(bench_format(Some("nvfp4")), "nvfp4");
        assert_eq!(bench_format(Some("mxfp4")), "mxfp4_triton");
        assert_eq!(bench_format(Some("none")), "bf16");
        assert_eq!(bench_format(None), "bf16");
    }
}
