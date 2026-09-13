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

use crate::cache_pools::{self, PoolGeometry};
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
        Pool::ALL.iter().copied().filter(|p| cache_pools::present(&geo, *p)).collect();
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
        let PoolGeometry { current, max, .. } = cache_pools::geometry(&geo, *pool);
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
        detail.push(Span::styled(cache_pools::note(&geo, *pool, shown), t.muted()));
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

    let facts = facts(geo);
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

/// The VRAM pane's footnotes: what the engine says about eviction, the window ratio and
/// the thinking gears the served template exposes.
pub fn facts(geo: &CacheGeometry) -> Vec<String> {
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
    facts
}

/// Total pool bytes if every pending edit were applied.
pub fn proposed_bytes(app: &App, geo: &CacheGeometry) -> u64 {
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

/// One line describing the engine's most recent pool rebuild, when it has done one.
pub fn last_rebuild_summary(app: &App) -> Option<String> {
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
    let g = cache_pools::geometry(&geo, pool);
    let from = app.cache_view.pending_for(pool).unwrap_or(g.current);
    let step = ((g.max as f64 * percent).abs().round() as u64).max(1);
    let next = if percent < 0.0 { from.saturating_sub(step) } else { from.saturating_add(step) };
    let next = g.clamp(next);
    if next == g.current {
        app.cache_view.set_pending(pool, None);
    } else {
        app.cache_view.set_pending(pool, Some(next));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delta_bytes_shows_a_sign_in_both_directions() {
        assert_eq!(DeltaBytes(2048).to_string(), "+2.00 KiB");
        assert_eq!(DeltaBytes(-2048).to_string(), "-2.00 KiB");
    }
}
