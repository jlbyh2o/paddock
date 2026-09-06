//! Serde mirrors of FreeToken's control-plane JSON.
//!
//! Every field is optional or defaulted: these documents grow between FreeToken
//! releases, and a manager that 500s on an unfamiliar key is worse than one that shows
//! a dash. Unknown fields are ignored rather than rejected for the same reason.

use serde::Deserialize;

// ---------------------------------------------------------------- /health

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Health {
    /// `loading`, `ok`, or `error`.
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub uptime_s: Option<u64>,
    /// `serving`, `loading`, or `rebuilding`.
    #[serde(default)]
    pub maintenance: Option<String>,
    /// Weight-loading phase, present only while `status == "loading"`.
    #[serde(default)]
    pub phase: Option<String>,
    #[serde(default)]
    pub progress: Option<LoadProgress>,
}

impl Health {
    pub fn is_ready(&self) -> bool {
        self.status == "ok"
    }
    pub fn is_loading(&self) -> bool {
        self.status == "loading"
    }
    pub fn is_error(&self) -> bool {
        self.status == "error"
    }
    /// Load completion in `0.0..=1.0`, or `None` when the total is not yet known.
    pub fn load_ratio(&self) -> Option<f64> {
        let p = self.progress.as_ref()?;
        (p.total_bytes > 0).then(|| crate::util::ratio(p.done_bytes, p.total_bytes))
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct LoadProgress {
    #[serde(default)]
    pub done_bytes: u64,
    #[serde(default)]
    pub total_bytes: u64,
}

// ---------------------------------------------------------------- /v1/stats

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Stats {
    #[serde(default)]
    pub model: ModelCard,
    #[serde(default)]
    pub uptime_s: u64,
    #[serde(default)]
    pub kv: Option<PagePool>,
    #[serde(default)]
    pub mamba: Option<SlotPool>,
    #[serde(default)]
    pub swa: Option<PagePool>,
    #[serde(default)]
    pub vram_bytes: u64,
    #[serde(default)]
    pub gpus: Vec<GpuCard>,
    #[serde(default)]
    pub throughput: Throughput,
    #[serde(default)]
    pub requests: RequestStats,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ModelCard {
    #[serde(default)]
    pub id: Option<String>,
    /// Context length in tokens.
    #[serde(default)]
    pub ctx: u64,
    /// `mha`, `hybrid_linear`, or `hybrid_swa`.
    #[serde(default)]
    pub attn: Option<String>,
    #[serde(default)]
    pub moe: bool,
    /// The checkpoint's recommended sampling parameters, when the server reports them.
    #[serde(default)]
    pub sampling: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct PagePool {
    #[serde(default)]
    pub used_pages: u64,
    #[serde(default)]
    pub total_pages: u64,
    #[serde(default = "one")]
    pub page_size: u64,
}

impl PagePool {
    pub fn used_tokens(&self) -> u64 {
        self.used_pages * self.page_size.max(1)
    }
    pub fn total_tokens(&self) -> u64 {
        self.total_pages * self.page_size.max(1)
    }
    pub fn ratio(&self) -> f64 {
        crate::util::ratio(self.used_pages, self.total_pages)
    }
}

fn one() -> u64 {
    1
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct SlotPool {
    #[serde(default)]
    pub used_slots: u64,
    #[serde(default)]
    pub total_slots: u64,
}

impl SlotPool {
    pub fn ratio(&self) -> f64 {
        crate::util::ratio(self.used_slots, self.total_slots)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct GpuCard {
    #[serde(default)]
    pub index: Option<u32>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub uuid: Option<String>,
    #[serde(default)]
    pub total_bytes: u64,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct Throughput {
    #[serde(default)]
    pub decode_tps: f64,
    #[serde(default)]
    pub prefill_tps: f64,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct RequestStats {
    #[serde(default)]
    pub active: u64,
    #[serde(default)]
    pub completed: u64,
    #[serde(default)]
    pub p95_ms: u64,
    #[serde(default)]
    pub ttft_mean_ms: u64,
    #[serde(default)]
    pub prompt_tokens_total: u64,
    #[serde(default)]
    pub completion_tokens_total: u64,
}

// ---------------------------------------------------------------- /v1/cache/status

#[derive(Debug, Clone, Default, Deserialize)]
pub struct CacheStatus {
    /// `serving`, `loading`, or `rebuilding`.
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub last_rebuild: Option<serde_json::Value>,
    #[serde(default)]
    pub geometry: CacheGeometry,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct CacheGeometry {
    #[serde(default)]
    pub num_pages: u64,
    #[serde(default = "one")]
    pub page_size: u64,
    #[serde(default)]
    pub moe_cache_size: u64,
    #[serde(default)]
    pub num_mamba_slots: u64,
    #[serde(default)]
    pub num_experts: u64,
    #[serde(default)]
    pub num_moe_layers: u64,
    #[serde(default)]
    pub moe_cache_policy: Option<String>,
    #[serde(default)]
    pub unit_bytes: UnitBytes,
    #[serde(default)]
    pub swa_full_tokens_ratio: f64,
    #[serde(default)]
    pub swa_page_size: u64,
    #[serde(default)]
    pub num_swa_pages: u64,
    #[serde(default)]
    pub cache_budget_bytes: u64,
    #[serde(default)]
    pub limits: Option<serde_json::Value>,
    #[serde(default)]
    pub reasoning: Option<Reasoning>,
}

impl CacheGeometry {
    /// Total expert slots the model has, which is the ceiling for the MoE cache.
    pub fn total_experts(&self) -> u64 {
        self.num_experts * self.num_moe_layers
    }

    /// VRAM the currently configured pools occupy, derived from the per-unit costs the
    /// engine reports. Zero when the engine has not published `unit_bytes` yet.
    pub fn pool_bytes(&self) -> PoolBytes {
        let u = &self.unit_bytes;
        PoolBytes {
            kv: self.num_pages * self.page_size.max(1) * u.kv_per_token,
            moe: self.moe_cache_size * u.moe_per_expert,
            mamba: self.num_mamba_slots * u.mamba_per_slot,
            swa: self.num_swa_pages * self.swa_page_size.max(1) * u.swa_per_token,
        }
    }

    /// Read an integer bound out of the server's `limits` block, when present.
    pub fn limit(&self, pool: &str, bound: &str) -> Option<u64> {
        self.limits.as_ref()?.get(pool)?.get(bound)?.as_u64()
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PoolBytes {
    pub kv: u64,
    pub moe: u64,
    pub mamba: u64,
    pub swa: u64,
}

impl PoolBytes {
    pub fn total(&self) -> u64 {
        self.kv + self.moe + self.mamba + self.swa
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct UnitBytes {
    #[serde(default)]
    pub kv_per_token: u64,
    #[serde(default)]
    pub moe_per_expert: u64,
    #[serde(default)]
    pub mamba_per_slot: u64,
    #[serde(default)]
    pub swa_per_token: u64,
}

/// The thinking gears the served checkpoint's chat template exposes.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Reasoning {
    #[serde(default)]
    pub gears: Vec<String>,
    #[serde(default)]
    pub default: Option<String>,
}

// ---------------------------------------------------------------- /v1/requests

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RequestPage {
    #[serde(default)]
    pub entries: Vec<RequestRecord>,
    #[serde(default)]
    pub next_cursor: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RequestRecord {
    #[serde(default)]
    pub ts: String,
    #[serde(default)]
    pub method: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub status: u16,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub ttft_ms: Option<u64>,
    #[serde(default)]
    pub prompt_tokens: Option<u64>,
    #[serde(default)]
    pub completion_tokens: Option<u64>,
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub error: Option<String>,
}

// ---------------------------------------------------------------- bench profile

/// `~/.cache/freetoken/benchbw/<gpu-uuid>.json`, written by `ft bench bw`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BenchProfile {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub timestamp: Option<String>,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub gpu: BenchGpu,
    #[serde(default)]
    pub cpu: BenchCpu,
    #[serde(default)]
    pub threshold: f64,
    #[serde(default)]
    pub ceilings: BenchCeilings,
    /// Per-dtype verdict: format -> `offload` | `hybrid`.
    #[serde(default)]
    pub dtypes: std::collections::BTreeMap<String, Option<String>>,
    #[serde(default)]
    pub dtype_kernels: std::collections::BTreeMap<String, BenchKernel>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct BenchGpu {
    #[serde(default)]
    pub index: Option<u32>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub uuid: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct BenchCpu {
    #[serde(default)]
    pub physical_cores: u32,
    #[serde(default)]
    pub threads_used: u32,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct BenchCeilings {
    #[serde(default)]
    pub cpu_stream_read_gbs: f64,
    #[serde(default)]
    pub pcie_linear_h2d_gbs: f64,
    #[serde(default)]
    pub pcie_linear_d2h_gbs: f64,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct BenchKernel {
    #[serde(default)]
    pub cpu_moe_gbs: Option<f64>,
    #[serde(default)]
    pub cpu_moe_isa: Option<String>,
    #[serde(default)]
    pub pcie_gather_gbs: Option<f64>,
    #[serde(default)]
    pub cpu_moe_overlap_gbs: Option<f64>,
    #[serde(default)]
    pub pcie_gather_overlap_gbs: Option<f64>,
    #[serde(default)]
    pub ratio: Option<f64>,
    #[serde(default)]
    pub recommended: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}
