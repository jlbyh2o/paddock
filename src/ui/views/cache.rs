//! The Cache view: live pool resizing without restarting the engine.
//!
//! This is FreeToken's elastic-memory feature made operable. Each pool gets a row with
//! its current size, a pending edit, and the VRAM that edit costs — because the whole
//! point of the trade is that experts and KV compete for the same budget, and the number
//! that matters is what the change does to the total, not to one pool in isolation.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use crate::ft::types::CacheGeometry;
use crate::ui::app::{App, Pool};
use crate::ui::theme::bar;
use crate::util::{bytes, count, ratio};

pub fn render(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(10), Constraint::Length(9)])
        .split(area);

    pools(f, app, rows[0]);
    budget(f, app, rows[1]);
}

fn pools(f: &mut Frame, app: &mut App, area: Rect) {
    let t = &app.theme;
    let title = match app.telemetry.cache.as_ref().map(|c| c.state.as_str()) {
        Some("rebuilding") => "Pools (rebuilding…)",
        _ if app.cache_view.applying => "Pools (applying…)",
        _ => "Pools",
    };
    let block = t.pane(title, true);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some(geo) = app.telemetry.cache.as_ref().map(|c| c.geometry.clone()) else {
        f.render_widget(
            Paragraph::new(
                "Cache geometry is only available while the engine is serving.\n\nStart an engine \
                 from the Serve tab, then come back here to retune the pools without a restart.",
            )
            .style(t.muted())
            .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    };

    let available: Vec<Pool> =
        Pool::ALL.iter().copied().filter(|p| pool_present(&geo, *p)).collect();
    if available.is_empty() {
        f.render_widget(
            Paragraph::new("This model exposes no resizable pools.").style(t.muted()),
            inner,
        );
        return;
    }

    app.cache_view.sel.clamp(available.len());
    let selected = app.cache_view.sel.index;
    let bar_w = (inner.width.saturating_sub(56)).clamp(8, 32) as usize;

    let mut lines: Vec<Line> = Vec::new();
    for (i, pool) in available.iter().enumerate() {
        let is_sel = i == selected;
        let current = pool_current(&geo, *pool);
        let max = pool_max(&geo, *pool).max(current.max(1));
        let pending = app.cache_view.pending_for(*pool);
        let shown = pending.unwrap_or(current);

        lines.push(Line::from(vec![
            Span::styled(if is_sel { "▌" } else { " " }, Style::default().fg(t.accent)),
            Span::styled(
                format!("{:<18}", pool.label()),
                if is_sel { t.selected() } else { t.text() },
            ),
            Span::styled("▕", t.muted()),
            Span::styled(
                bar(ratio(shown, max), bar_w),
                Style::default().fg(if pending.is_some() { t.warn } else { t.gauge }),
            ),
            Span::styled("▏ ", t.muted()),
            Span::styled(
                format!("{:>10}", count(shown)),
                if pending.is_some() {
                    Style::default().fg(t.warn).add_modifier(Modifier::BOLD)
                } else {
                    t.value()
                },
            ),
            Span::styled(format!(" {}", pool.unit()), t.muted()),
        ]));

        let mut detail = vec![Span::raw("                   ")];
        if let Some(p) = pending {
            let delta = p as i64 - current as i64;
            detail.push(Span::styled(
                format!("was {}  ({delta:+})   ", count(current)),
                Style::default().fg(t.warn),
            ));
        }
        detail.push(Span::styled(format!("max {}   ", count(max)), t.muted()));
        detail.push(Span::styled(pool_note(&geo, *pool, shown), t.muted()));
        lines.push(Line::from(detail));
    }

    f.render_widget(Paragraph::new(lines), inner);
}

fn budget(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let block = t.pane("VRAM budget", false);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some(geo) = app.telemetry.cache.as_ref().map(|c| &c.geometry) else {
        f.render_widget(Paragraph::new("").style(t.muted()), inner);
        return;
    };

    let current = geo.pool_bytes();
    let proposed = proposed_bytes(app, geo);
    let budget = geo.cache_budget_bytes;

    let mut lines = vec![Line::from(vec![
        Span::styled(format!("{:<18}", "Current pools"), t.label()),
        Span::styled(bytes(current.total()), t.value()),
        Span::styled(
            format!(
                "   KV {}  MoE {}  GDN {}  SWA {}",
                bytes(current.kv),
                bytes(current.moe),
                bytes(current.mamba),
                bytes(current.swa)
            ),
            t.muted(),
        ),
    ])];

    if app.cache_view.has_pending() {
        let over = budget > 0 && proposed > budget;
        lines.push(Line::from(vec![
            Span::styled(format!("{:<18}", "After rebuild"), t.label()),
            Span::styled(
                bytes(proposed),
                Style::default().fg(if over { t.bad } else { t.warn }).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("   {:+}", DeltaBytes(proposed as i64 - current.total() as i64)),
                t.muted(),
            ),
        ]));
        if over {
            lines.push(Line::from(Span::styled(
                "Exceeds the engine's cache budget — the rebuild will be rejected.",
                Style::default().fg(t.bad),
            )));
        }
    }

    if budget > 0 {
        let used = if app.cache_view.has_pending() { proposed } else { current.total() };
        let r = ratio(used, budget);
        lines.push(Line::from(vec![
            Span::styled(format!("{:<18}", "Budget"), t.label()),
            Span::styled("▕", t.muted()),
            Span::styled(bar(r, 30), Style::default().fg(t.for_ratio(r))),
            Span::styled("▏ ", t.muted()),
            Span::styled(bytes(budget), t.muted()),
        ]));
    }

    let mut facts: Vec<String> = Vec::new();
    if let Some(policy) = &geo.moe_cache_policy {
        facts.push(format!("expert eviction: {policy}"));
    }
    if geo.swa_full_tokens_ratio > 0.0 {
        facts.push(format!("window/full ratio: {:.2}", geo.swa_full_tokens_ratio));
    }
    if let Some(r) = &geo.reasoning {
        if !r.gears.is_empty() {
            let default = r.default.as_deref().unwrap_or("—");
            facts.push(format!("thinking gears: {} (default {default})", r.gears.join("/")));
        }
    }
    if !facts.is_empty() {
        lines.push(Line::from(Span::styled(facts.join("   ·   "), t.muted())));
    }
    if let Some(last) = last_rebuild_summary(app) {
        lines.push(Line::from(Span::styled(last, t.muted())));
    }

    lines.push(Line::from(""));
    if app.cache_view.has_pending() {
        lines.push(Line::from(Span::styled(
            "Press a to apply. The engine must be idle — a rebuild with requests in flight \
             is rejected rather than queued.",
            t.muted(),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            "←/→ adjust by 1%, Shift+←/→ by 10%, r resets a pool, a applies.",
            t.muted(),
        )));
    }

    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
}

/// Total pool bytes if every pending edit were applied.
fn proposed_bytes(app: &App, geo: &CacheGeometry) -> u64 {
    let u = &geo.unit_bytes;
    let moe = app.cache_view.pending_for(Pool::Moe).unwrap_or(geo.moe_cache_size);
    let kv = app.cache_view.pending_for(Pool::Kv).unwrap_or(geo.num_pages);
    let mamba = app.cache_view.pending_for(Pool::Mamba).unwrap_or(geo.num_mamba_slots);
    let swa = app.cache_view.pending_for(Pool::Swa).unwrap_or(geo.num_swa_pages);
    moe * u.moe_per_expert
        + kv * geo.page_size.max(1) * u.kv_per_token
        + mamba * u.mamba_per_slot
        + swa * geo.swa_page_size.max(1) * u.swa_per_token
}

pub fn pool_present(geo: &CacheGeometry, pool: Pool) -> bool {
    match pool {
        Pool::Moe => geo.moe_cache_size > 0 || geo.total_experts() > 0,
        Pool::Kv => geo.num_pages > 0,
        Pool::Mamba => geo.num_mamba_slots > 0,
        Pool::Swa => geo.num_swa_pages > 0 && geo.swa_page_size > 0,
    }
}

pub fn pool_current(geo: &CacheGeometry, pool: Pool) -> u64 {
    match pool {
        Pool::Moe => geo.moe_cache_size,
        Pool::Kv => geo.num_pages,
        Pool::Mamba => geo.num_mamba_slots,
        Pool::Swa => geo.num_swa_pages,
    }
}

/// The upper bound for a pool. The server publishes limits sized against the real cache
/// budget; fall back to something defensible when it does not.
///
/// Two things have to line up with the engine. The key is the one FreeToken publishes
/// (`_LIMIT_KEYS` in its `cache_report.py`), and the unit is the one it denominates that
/// bound in: tokens for the paged pools, slots for the others. ft-man sizes every pool in
/// the unit `/v1/cache/rebuild` accepts, which for KV and the window is *pages*, so those
/// two convert. The conversion is invisible on a model with `page_size` 1 and is a factor
/// of 128 on DSV4.
pub fn pool_max(geo: &CacheGeometry, pool: Pool) -> u64 {
    let (key, tokens_per_unit) = match pool {
        Pool::Moe => ("moe_experts", 1),
        Pool::Kv => ("kv_tokens", geo.page_size.max(1)),
        Pool::Mamba => ("mamba_slots", 1),
        Pool::Swa => ("swa_tokens", geo.swa_page_size.max(1)),
    };
    if let Some(max) = geo.limit(key, "max").filter(|m| *m > 0) {
        return (max / tokens_per_unit).max(1);
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
fn pool_note(geo: &CacheGeometry, pool: Pool, value: u64) -> String {
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

/// One line describing the engine's most recent pool rebuild, when it has done one.
fn last_rebuild_summary(app: &App) -> Option<String> {
    let last = app.telemetry.cache.as_ref()?.last_rebuild.as_ref()?.as_object()?;
    let mut parts: Vec<String> = Vec::new();
    for (key, label) in [
        ("moe_cache_size", "MoE"),
        ("num_pages", "KV"),
        ("mamba_slots", "GDN"),
        ("num_swa_pages", "SWA"),
    ] {
        if let Some(v) = last.get(key).and_then(serde_json::Value::as_u64) {
            parts.push(format!("{label} {}", count(v)));
        }
    }
    (!parts.is_empty()).then(|| format!("last rebuild: {}", parts.join("  ")))
}

/// Format a signed byte delta, which `bytes()` cannot do because it takes a `u64`.
struct DeltaBytes(i64);

impl std::fmt::Display for DeltaBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let sign = if self.0 < 0 { "-" } else { "+" };
        write!(f, "{sign}{}", bytes(self.0.unsigned_abs()))
    }
}

/// Nudge a pool's pending value. Steps are proportional so the same keystroke is useful
/// whether the pool holds 64 slots or 400,000 pages.
pub fn adjust(app: &mut App, pool: Pool, percent: f64) {
    let Some(geo) = app.telemetry.cache.as_ref().map(|c| c.geometry.clone()) else { return };
    let current = app.cache_view.pending_for(pool).unwrap_or_else(|| pool_current(&geo, pool));
    let max = pool_max(&geo, pool);
    let step = ((max as f64 * percent).abs().round() as u64).max(1);
    let next = if percent < 0.0 { current.saturating_sub(step) } else { (current + step).min(max) };
    let next = next.max(1);
    if next == pool_current(&geo, pool) {
        app.cache_view.set_pending(pool, None);
    } else {
        app.cache_view.set_pending(pool, Some(next));
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
        assert_eq!(pool_max(&geo(), Pool::Moe), 128 * 48);
    }

    #[test]
    fn a_pool_with_no_slots_is_hidden() {
        let g = geo();
        assert!(pool_present(&g, Pool::Moe));
        assert!(pool_present(&g, Pool::Kv));
        assert!(!pool_present(&g, Pool::Mamba));
        assert!(!pool_present(&g, Pool::Swa));
    }

    /// The keys are FreeToken's, not ft-man's own names for the pools: a mismatch here is
    /// silent, because every lookup simply misses and falls back to the local estimate.
    #[test]
    fn server_published_limits_win_over_the_fallback() {
        let mut g = geo();
        g.limits = Some(serde_json::json!({"moe_experts": {"min": 128, "max": 900}}));
        assert_eq!(pool_max(&g, Pool::Moe), 900);
    }

    /// KV and the window are published in tokens but sized in pages everywhere else, so a
    /// paged model must not get a bound `page_size` times too generous.
    #[test]
    fn a_paged_pool_converts_the_published_token_bound_into_pages() {
        let mut g = geo();
        g.page_size = 128;
        g.limits = Some(serde_json::json!({"kv_tokens": {"min": 128, "max": 262144}}));
        assert_eq!(pool_max(&g, Pool::Kv), 2048);
    }

    #[test]
    fn delta_bytes_shows_a_sign_in_both_directions() {
        assert_eq!(DeltaBytes(2048).to_string(), "+2.00 KiB");
        assert_eq!(DeltaBytes(-2048).to_string(), "-2.00 KiB");
    }
}
