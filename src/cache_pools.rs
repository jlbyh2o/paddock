//! Pool geometry: bounds, current size and unit for each resizable cache pool.
//!
//! Two things have to line up with the engine and neither is visible when it goes wrong.
//! The limit key is the one FreeToken publishes (`_LIMIT_KEYS` in its `cache_report.py`),
//! and the unit is the one it denominates that bound in: tokens for the paged pools, slots
//! for the others. paddock sizes every pool in the unit `/v1/cache/rebuild` accepts, which
//! for KV and the window is *pages*, so those two convert — invisible on a model with
//! `page_size` 1 and a factor of 128 on DSV4.
//!
//! So the arithmetic lives here once. The Cache view draws from it, the web snapshot sends
//! it, and [`crate::actions`] clamps against it; three implementations of the same
//! conversion would disagree the first time one of them was changed.

use crate::ft::types::CacheGeometry;
use crate::ui::app::Pool;
use crate::util::{count, ratio};

/// One pool's bounds and current size, every number already in the pool's own unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolGeometry {
    pub current: u64,
    /// The effective lower bound, never below 1: a pool resized to zero is a pool the
    /// engine no longer has.
    pub min: u64,
    /// The effective upper bound, never below `current` and never below 1.
    pub max: u64,
    pub unit: &'static str,
}

impl PoolGeometry {
    /// Bring a requested size inside the bounds. `min` is applied first and `max` wins,
    /// so a degenerate geometry cannot produce an empty range.
    pub fn clamp(&self, value: u64) -> u64 {
        value.clamp(self.min.min(self.max), self.max)
    }
}

/// Everything the UI and the actions need about one pool, in one read of the geometry.
pub fn geometry(geo: &CacheGeometry, pool: Pool) -> PoolGeometry {
    let current = current(geo, pool);
    let max = published_max(geo, pool).max(current).max(1);
    let min = published_min(geo, pool).unwrap_or(1).max(1).min(max);
    PoolGeometry { current, min, max, unit: pool.unit() }
}

/// Whether this model exposes the pool at all.
pub fn present(geo: &CacheGeometry, pool: Pool) -> bool {
    match pool {
        Pool::Moe => geo.moe_cache_size > 0 || geo.total_experts() > 0,
        Pool::Kv => geo.num_pages > 0,
        Pool::Mamba => geo.num_mamba_slots > 0,
        Pool::Swa => geo.num_swa_pages > 0 && geo.swa_page_size > 0,
    }
}

/// The pool's size right now, in its own unit.
pub fn current(geo: &CacheGeometry, pool: Pool) -> u64 {
    match pool {
        Pool::Moe => geo.moe_cache_size,
        Pool::Kv => geo.num_pages,
        Pool::Mamba => geo.num_mamba_slots,
        Pool::Swa => geo.num_swa_pages,
    }
}

/// FreeToken's own name for a pool's bounds, as its `cache_report.py` publishes them.
pub fn limit_key(pool: Pool) -> &'static str {
    match pool {
        Pool::Moe => "moe_experts",
        Pool::Kv => "kv_tokens",
        Pool::Mamba => "mamba_slots",
        Pool::Swa => "swa_tokens",
    }
}

/// How many published tokens make one of the units paddock sizes this pool in. The paged
/// pools are published in tokens and rebuilt in pages; the others are one to one.
pub fn tokens_per_unit(geo: &CacheGeometry, pool: Pool) -> u64 {
    match pool {
        Pool::Moe | Pool::Mamba => 1,
        Pool::Kv => geo.page_size.max(1),
        Pool::Swa => geo.swa_page_size.max(1),
    }
}

/// The engine's published lower bound, in the pool's own unit. `None` when it published
/// none — the TUI never shows a minimum, but a web slider needs one to clamp against.
fn published_min(geo: &CacheGeometry, pool: Pool) -> Option<u64> {
    geo.limit(limit_key(pool), "min").map(|min| min / tokens_per_unit(geo, pool))
}

/// The engine's published upper bound, in the pool's own unit, or a defensible local
/// fallback when it published none.
fn published_max(geo: &CacheGeometry, pool: Pool) -> u64 {
    if let Some(max) = geo.limit(limit_key(pool), "max").filter(|m| *m > 0) {
        return (max / tokens_per_unit(geo, pool)).max(1);
    }
    match pool {
        // An expert cache larger than the model's total expert count is pointless.
        Pool::Moe => geo.total_experts().max(geo.moe_cache_size),
        Pool::Kv => geo.num_pages.saturating_mul(4).max(1),
        Pool::Mamba => geo.num_mamba_slots.saturating_mul(4).max(1),
        Pool::Swa => geo.num_pages.saturating_mul(geo.page_size.max(1)).max(geo.num_swa_pages),
    }
}

/// A short explanation of what a given size means in practice.
pub fn note(geo: &CacheGeometry, pool: Pool, value: u64) -> String {
    match pool {
        Pool::Moe => {
            let total = geo.total_experts();
            if total == 0 {
                return String::new();
            }
            format!("{:.0}% of {} experts resident", ratio(value, total) * 100.0, count(total))
        }
        Pool::Kv => format!("{} tokens", count(value * geo.page_size.max(1))),
        Pool::Mamba => String::new(),
        Pool::Swa => format!("{} tokens of window", count(value * geo.swa_page_size.max(1))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ft::types::UnitBytes;

    fn geo() -> CacheGeometry {
        CacheGeometry {
            num_pages: 8192,
            page_size: 1,
            moe_cache_size: 512,
            num_experts: 128,
            num_moe_layers: 48,
            unit_bytes: UnitBytes {
                kv_per_token: 1024,
                moe_per_expert: 1 << 20,
                ..Default::default()
            },
            cache_budget_bytes: 8 << 30,
            ..Default::default()
        }
    }

    #[test]
    fn moe_capacity_is_capped_at_the_models_expert_count() {
        assert_eq!(geometry(&geo(), Pool::Moe).max, 128 * 48);
    }

    #[test]
    fn a_pool_with_no_slots_is_hidden() {
        let g = geo();
        assert!(present(&g, Pool::Moe));
        assert!(present(&g, Pool::Kv));
        assert!(!present(&g, Pool::Mamba));
        assert!(!present(&g, Pool::Swa));
    }

    /// The keys are FreeToken's, not paddock's own names for the pools: a mismatch here is
    /// silent, because every lookup simply misses and falls back to the local estimate.
    #[test]
    fn server_published_limits_win_over_the_fallback() {
        let mut g = geo();
        g.limits = Some(serde_json::json!({"moe_experts": {"min": 128, "max": 900}}));
        let p = geometry(&g, Pool::Moe);
        assert_eq!(p.max, 900);
        assert_eq!(p.min, 128);
    }

    /// KV and the window are published in tokens but sized in pages everywhere else, so a
    /// paged model must not get a bound `page_size` times too generous.
    #[test]
    fn a_paged_pool_converts_the_published_token_bound_into_pages() {
        let mut g = geo();
        g.page_size = 128;
        g.num_pages = 1024;
        g.limits = Some(serde_json::json!({"kv_tokens": {"min": 128, "max": 262144}}));
        let p = geometry(&g, Pool::Kv);
        assert_eq!(p.max, 2048, "262,144 tokens is 2,048 pages of 128");
        assert_eq!(p.min, 1, "128 tokens is one page");
    }

    /// The minimum is never null and never zero: a slider needs a floor, and a pool of
    /// zero pages is not something the engine can rebuild into.
    #[test]
    fn the_minimum_is_always_at_least_one_and_never_above_the_maximum() {
        let mut g = geo();
        assert_eq!(geometry(&g, Pool::Kv).min, 1, "no published bound still floors at 1");

        g.num_pages = 2;
        g.limits = Some(serde_json::json!({"kv_tokens": {"min": 0, "max": 4}}));
        let p = geometry(&g, Pool::Kv);
        assert_eq!(p.min, 1);
        assert!(p.min <= p.max);

        // A nonsense pair (min above max) must still clamp into a usable range.
        g.limits = Some(serde_json::json!({"kv_tokens": {"min": 99999, "max": 4}}));
        let p = geometry(&g, Pool::Kv);
        assert_eq!(p.clamp(1), 1.max(p.min.min(p.max)));
        assert!(p.clamp(u64::MAX) <= p.max);
    }

    #[test]
    fn clamping_holds_a_request_inside_the_published_bounds() {
        let mut g = geo();
        g.limits = Some(serde_json::json!({"moe_experts": {"min": 128, "max": 900}}));
        let p = geometry(&g, Pool::Moe);
        assert_eq!(p.clamp(1), 128);
        assert_eq!(p.clamp(500), 500);
        assert_eq!(p.clamp(100_000), 900);
    }
}
