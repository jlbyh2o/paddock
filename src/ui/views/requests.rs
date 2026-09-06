//! The Requests view: the server's in-memory request ring.
//!
//! This is the fastest way to answer "is the agent actually reaching the engine, and
//! what is it costing?" — so the table leads with status, latency and token counts, and
//! colors anything that is not a 2xx.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use crate::ui::app::App;
use crate::util::{count, truncate};

pub fn render(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = if app.requests_view.show_details {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(6), Constraint::Length(10)])
            .split(area)
    } else {
        Layout::default().constraints([Constraint::Min(1)]).split(area)
    };

    table(f, app, rows[0]);
    if app.requests_view.show_details {
        detail(f, app, rows[1]);
    }
}

fn table(f: &mut Frame, app: &mut App, area: Rect) {
    let t = &app.theme;
    let n = app.requests_view.entries.len();
    let title = if app.requests_view.paused {
        format!("Requests ({n}) — paused")
    } else {
        format!("Requests ({n})")
    };
    let block = t.pane(title, true);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if n == 0 {
        let msg = if app.server_reachable() {
            "No requests yet.\n\nEvery call the engine serves shows up here — chat completions, \
             Anthropic messages, health probes from an agent, all of it."
        } else {
            "The request log comes from the running server; nothing to show while it is down."
        };
        f.render_widget(Paragraph::new(msg).style(t.muted()).wrap(Wrap { trim: true }), inner);
        return;
    }

    let header = Line::from(vec![
        Span::styled(" ", t.header()),
        Span::styled(format!("{:<9}", "TIME"), t.header()),
        Span::styled(format!("{:<7}", "METHOD"), t.header()),
        Span::styled(format!("{:<28}", "PATH"), t.header()),
        Span::styled(format!("{:>5}", "CODE"), t.header()),
        Span::styled(format!("{:>9}", "DUR"), t.header()),
        Span::styled(format!("{:>8}", "TTFT"), t.header()),
        Span::styled(format!("{:>9}", "IN"), t.header()),
        Span::styled(format!("{:>9}", "OUT"), t.header()),
    ]);

    let height = (inner.height as usize).saturating_sub(1);
    app.requests_view.sel.clamp(n);
    // Newest last, and the cursor starts pinned to the newest row.
    let range = app.requests_view.sel.window(n, height);
    let selected = app.requests_view.sel.index;

    let mut lines = vec![header];
    for i in range {
        let r = &app.requests_view.entries[i];
        let is_sel = i == selected;
        let status_color = match r.status {
            200..=299 => t.good,
            300..=399 => t.dim,
            400..=499 => t.warn,
            _ => t.bad,
        };
        let base = if is_sel { t.selected() } else { t.text() };
        lines.push(Line::from(vec![
            Span::styled(if is_sel { "▌" } else { " " }, Style::default().fg(t.accent)),
            Span::styled(format!("{:<9}", clock(&r.ts)), if is_sel { base } else { t.muted() }),
            Span::styled(format!("{:<7}", r.method), if is_sel { base } else { t.muted() }),
            Span::styled(format!("{:<28}", truncate(&r.path, 27)), base),
            Span::styled(
                format!("{:>5}", r.status),
                Style::default().fg(status_color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("{:>9}", ms(r.duration_ms)), base),
            Span::styled(
                format!("{:>8}", r.ttft_ms.map(ms).unwrap_or_else(|| "—".into())),
                if is_sel { base } else { t.muted() },
            ),
            Span::styled(
                format!("{:>9}", r.prompt_tokens.map(count).unwrap_or_else(|| "—".into())),
                if is_sel { base } else { t.muted() },
            ),
            Span::styled(
                format!("{:>9}", r.completion_tokens.map(count).unwrap_or_else(|| "—".into())),
                if is_sel { base } else { t.muted() },
            ),
        ]));
    }

    f.render_widget(Paragraph::new(lines), inner);
}

fn detail(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let block = t.pane("Request detail", false);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some(r) = app.requests_view.entries.get(app.requests_view.sel.index) else {
        f.render_widget(Paragraph::new("").style(t.muted()), inner);
        return;
    };

    let mut lines = vec![
        t.field("Timestamp", r.ts.clone()),
        t.field("Route", format!("{} {}", r.method, r.path)),
        t.field("Status", r.status.to_string()),
        t.field("Model", r.model.clone().unwrap_or_else(|| "—".into())),
        t.field("Duration", ms(r.duration_ms)),
    ];
    if let Some(ttft) = r.ttft_ms {
        lines.push(t.field("Time to first token", ms(ttft)));
    }
    match (r.prompt_tokens, r.completion_tokens) {
        (Some(p), Some(c)) => {
            lines.push(t.field("Tokens", format!("{} in, {} out", count(p), count(c))));
            // Decode rate is the number people actually compare between configurations,
            // and it is not in the record — derive it rather than making them do it.
            if r.duration_ms > 0 && c > 0 {
                let secs = r.duration_ms as f64 / 1000.0;
                lines.push(t.field("Decode rate", format!("{:.1} tok/s", c as f64 / secs)));
            }
        }
        _ => lines.push(t.field("Tokens", "—")),
    }
    if let Some(s) = r.stream {
        lines.push(t.field("Streamed", if s { "yes" } else { "no" }));
    }
    if let Some(e) = &r.error {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(e.clone(), Style::default().fg(t.bad))));
    }

    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
}

/// Pull `HH:MM:SS` out of an ISO timestamp without a full parse.
fn clock(ts: &str) -> String {
    ts.split('T').nth(1).map(|t| t.chars().take(8).collect()).unwrap_or_else(|| truncate(ts, 8))
}

fn ms(v: u64) -> String {
    if v >= 10_000 {
        format!("{:.1} s", v as f64 / 1000.0)
    } else {
        format!("{v} ms")
    }
}

/// True when the cursor should stay glued to the newest row.
pub fn at_tail(app: &App) -> bool {
    let n = app.requests_view.entries.len();
    n == 0 || app.requests_view.sel.index + 1 >= n
}

/// Move the cursor to the newest entry, used when new records arrive while following.
pub fn follow_tail(app: &mut App) {
    let n = app.requests_view.entries.len();
    app.requests_view.sel.last(n);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ft::types::RequestRecord;

    #[test]
    fn clock_extracts_the_time_component() {
        assert_eq!(clock("2026-09-05T14:23:07.123456Z"), "14:23:07");
        assert_eq!(clock("nonsense"), "nonsense");
    }

    #[test]
    fn durations_switch_to_seconds_when_long() {
        assert_eq!(ms(340), "340 ms");
        assert_eq!(ms(12_500), "12.5 s");
    }

    #[test]
    fn record_defaults_render_without_panicking() {
        let r = RequestRecord::default();
        assert_eq!(clock(&r.ts), "");
    }
}
