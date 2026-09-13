//! The Hub view: search Hugging Face, pick files, download.
//!
//! Search results on the left; the selected repo's file list on the right, pre-filtered
//! to what a serving engine actually reads. Every file's selection is editable, because
//! the default heuristic cannot know that a particular repo ships three quantizations
//! and only one of them is wanted.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use crate::ui::app::{App, HubFocus};
use crate::util::{bytes, count, truncate};

pub fn render(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(6)])
        .split(area);

    search_bar(f, app, rows[0]);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(46), Constraint::Percentage(54)])
        .split(rows[1]);

    results(f, app, cols[0]);

    // The quantization pane only appears when there is a choice to make. A plain
    // safetensors checkpoint should not be given a list of one, and the space it would
    // take is the file list's on a small terminal.
    if app.hub_view.layout.is_multi() {
        let right = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(9), Constraint::Min(4), Constraint::Length(9)])
            .split(cols[1]);
        quantizations(f, app, right[0]);
        files(f, app, right[1]);
        target(f, app, right[2]);
    } else {
        let right = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(6), Constraint::Length(9)])
            .split(cols[1]);
        files(f, app, right[0]);
        target(f, app, right[1]);
    }
}

/// The quantization list: the question this tab exists to ask.
fn quantizations(f: &mut Frame, app: &mut App, area: Rect) {
    let t = &app.theme;
    let focused = app.hub_view.focus == HubFocus::Variants;
    let rows: Vec<(String, u64, usize)> =
        app.hub_view.layout.weights().map(|v| (v.label.clone(), v.bytes, v.file_count())).collect();

    let chosen = (!app.hub_view.custom_selection).then(|| app.hub_view.variant.clone()).flatten();
    let title = match &chosen {
        Some(v) => format!("Quantization — {v}"),
        None => format!("Quantization — {} available, none chosen", rows.len()),
    };
    let block = t.pane(title, focused);
    let inner = block.inner(area);
    f.render_widget(block, area);

    app.hub_view.variant_sel.clamp(rows.len());
    let height = inner.height as usize;
    let range = app.hub_view.variant_sel.window(rows.len(), height);
    let selected = app.hub_view.variant_sel.index;
    let name_w = inner.width.saturating_sub(24) as usize;

    let mut lines: Vec<Line> = Vec::new();
    for i in range {
        let (label, size, count) = &rows[i];
        let is_sel = i == selected && focused;
        let is_chosen = chosen.as_deref() == Some(label.as_str());
        let mark_style = if is_chosen { Style::default().fg(t.good) } else { t.muted() };
        let name_style = match (is_sel, is_chosen) {
            (true, _) => t.selected(),
            (false, true) => t.text(),
            (false, false) => t.muted(),
        };
        lines.push(Line::from(vec![
            Span::styled(if is_sel { "▌" } else { " " }, Style::default().fg(t.accent)),
            Span::styled(if is_chosen { "◉ " } else { "○ " }, mark_style),
            Span::styled(format!("{:<name_w$}", truncate(label, name_w)), name_style),
            Span::styled(format!("{:>10}", bytes(*size)), t.muted()),
            Span::styled(
                format!("{:>5}", if *count == 1 { "1 pt".into() } else { format!("{count} pts") }),
                t.muted(),
            ),
        ]));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn search_bar(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let title = if app.hub_view.searching {
        "Search Hugging Face (searching…)".to_string()
    } else if app.hub_token.is_some() {
        "Search Hugging Face (authenticated)".to_string()
    } else {
        "Search Hugging Face".to_string()
    };
    let block = t.pane(title, app.hub_view.editing);
    let inner = block.inner(area);
    f.render_widget(block, area);
    f.render_widget(
        Paragraph::new(app.hub_view.query.line(
            t,
            app.hub_view.editing,
            "press / to search, e.g. \"Qwen3.6 NVFP4\" or a full repo id",
        )),
        inner,
    );
}

fn results(f: &mut Frame, app: &mut App, area: Rect) {
    let t = &app.theme;
    let focused = app.hub_view.focus == HubFocus::Results && !app.hub_view.editing;
    let block = t.pane(format!("Results ({})", app.hub_view.results.len()), focused);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.hub_view.results.is_empty() {
        let mut lines = vec![Line::from(Span::styled("Search for a model to begin.", t.muted()))];
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "FreeToken's known-good checkpoints include Qwen3.6-35B-A3B, GLM-5.2, \
             gpt-oss-120b, Gemma-4 and DeepSeek-V4-Flash.",
            t.muted(),
        )));
        lines.push(Line::from(""));
        match &app.hub_token {
            Some(token) => lines.push(Line::from(Span::styled(
                format!("Authenticated with {}.", token.source),
                Style::default().fg(t.good),
            ))),
            None => lines.push(Line::from(Span::styled(
                "No Hugging Face token found — gated repos will not be downloadable. Set \
                 hub.token in the config file, or the HF_TOKEN environment variable.",
                Style::default().fg(t.warn),
            ))),
        }
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
        return;
    }

    // Two rows per result: id, then the metadata line.
    let per = 2usize;
    let capacity = (inner.height as usize) / per;
    app.hub_view.sel.clamp(app.hub_view.results.len());
    let range = app.hub_view.sel.window(app.hub_view.results.len(), capacity.max(1));
    let selected = app.hub_view.sel.index;
    let w = inner.width.saturating_sub(2) as usize;

    let mut lines: Vec<Line> = Vec::new();
    for i in range {
        let r = &app.hub_view.results[i];
        let is_sel = i == selected;
        let mut head = vec![
            Span::styled(if is_sel { "▌" } else { " " }, Style::default().fg(t.accent)),
            Span::styled(
                truncate(&r.id, w.saturating_sub(8)),
                if is_sel { t.selected() } else { t.text() },
            ),
        ];
        if r.is_gated() {
            head.push(Span::styled("  gated", Style::default().fg(t.warn)));
        }
        if r.private {
            head.push(Span::styled("  private", Style::default().fg(t.warn)));
        }
        lines.push(Line::from(head));

        let mut meta = format!("{:>9} ↓  {:>5} ♥  ", count(r.downloads), count(r.likes));
        if let Some(d) = r.last_modified.as_ref().and_then(|s| s.split('T').next()) {
            meta.push_str(&format!("{d}  "));
        }
        let tags = r.interesting_tags().join(" ");
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(meta, t.muted()),
            Span::styled(truncate(&tags, w.saturating_sub(34)), t.muted()),
        ]));
    }

    f.render_widget(Paragraph::new(lines), inner);
}

fn files(f: &mut Frame, app: &mut App, area: Rect) {
    let t = &app.theme;
    let focused = app.hub_view.focus == HubFocus::Files;

    let (selected_bytes, selected_count) = app.hub_view.selected();

    let title = if app.hub_view.loading_info {
        "Files (loading…)".to_string()
    } else if app.hub_view.files.is_empty() {
        "Files".to_string()
    } else {
        format!(
            "Files — {selected_count} of {} selected, {}",
            app.hub_view.files.len(),
            bytes(selected_bytes)
        )
    };
    let block = t.pane(title, focused);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.hub_view.files.is_empty() {
        let msg = if app.hub_view.loading_info {
            "Fetching the file listing…"
        } else {
            "Press Enter on a result to list its files."
        };
        f.render_widget(Paragraph::new(msg).style(t.muted()).wrap(Wrap { trim: true }), inner);
        return;
    }

    let height = inner.height as usize;
    app.hub_view.file_sel.clamp(app.hub_view.files.len());
    let range = app.hub_view.file_sel.window(app.hub_view.files.len(), height);
    let selected = app.hub_view.file_sel.index;
    let name_w = inner.width.saturating_sub(16) as usize;

    let mut lines: Vec<Line> = Vec::new();
    for i in range {
        let file = &app.hub_view.files[i];
        let is_sel = i == selected && focused;
        let mark = if file.wanted { "◉" } else { "○" };
        let mark_style = if file.wanted { Style::default().fg(t.good) } else { t.muted() };
        let name_style = match (is_sel, file.wanted) {
            (true, _) => t.selected(),
            (false, true) => t.text(),
            (false, false) => t.muted(),
        };
        lines.push(Line::from(vec![
            Span::styled(if is_sel { "▌" } else { " " }, Style::default().fg(t.accent)),
            Span::styled(format!("{mark} "), mark_style),
            Span::styled(format!("{:<name_w$}", truncate(&file.path, name_w)), name_style),
            Span::styled(
                format!("{:>10}", if file.size > 0 { bytes(file.size) } else { "—".into() }),
                t.muted(),
            ),
        ]));
    }

    f.render_widget(Paragraph::new(lines), inner);
}

fn target(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let (title, focused) = match (&app.hub_view.compat, &app.hub_view.compat_error) {
        (Some(r), _) => (format!("Compatibility — {}", r.verdict().label()), false),
        _ if app.hub_view.checking_compat => ("Compatibility — checking…".to_string(), false),
        (None, Some(_)) => ("Compatibility — could not check".to_string(), false),
        (None, None) => ("Download to".to_string(), false),
    };
    let block = t.pane(title, focused);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let w = inner.width.saturating_sub(2) as usize;
    let mut lines: Vec<Line> = Vec::new();

    if let Some(report) = &app.hub_view.compat {
        let color = match report.verdict() {
            crate::compat::Verdict::Supported => t.good,
            crate::compat::Verdict::Caution => t.warn,
            crate::compat::Verdict::Unsupported => t.bad,
            crate::compat::Verdict::Unknown => t.dim,
        };
        // An explicit separator, not padding: "not supported, with caveats" is longer
        // than any sensible column width and would otherwise run into the summary.
        let label = report.verdict().label();
        let head = vec![
            Span::styled(
                label.to_string(),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  ·  ", t.muted()),
            Span::styled(truncate(&report.summary(), w.saturating_sub(label.len() + 5)), t.muted()),
        ];
        lines.push(Line::from(head));

        for (level, note) in report.notes.iter().take(3) {
            let (mark, color) = match level {
                crate::compat::Level::Blocker => ("✗ ", t.bad),
                crate::compat::Level::Caution => ("! ", t.warn),
                crate::compat::Level::Info => ("· ", t.dim),
            };
            lines.push(Line::from(vec![
                Span::styled(mark, Style::default().fg(color)),
                Span::styled(note.clone(), Style::default().fg(color)),
            ]));
        }
        if report.notes.is_empty() {
            lines.push(Line::from(Span::styled(
                "Nothing known stands in the way. Judged from config.json only — a clean \
                 result is not a guarantee it will serve.",
                t.muted(),
            )));
        }
        lines.push(Line::from(""));
    }

    if let Some(err) = &app.hub_view.compat_error {
        lines.push(Line::from(Span::styled(
            format!("Could not read this repo's config.json: {err}"),
            Style::default().fg(t.warn),
        )));
        lines.push(Line::from(Span::styled(
            "A repo with no config.json (a GGUF-only build, for instance) cannot be judged \
             this way.",
            t.muted(),
        )));
        lines.push(Line::from(""));
    }

    if let Some(info) = &app.hub_view.info {
        let mut meta = format!("{} @ {}", info.id, app.hub_view.revision);
        if let Some(sha) = &info.sha {
            meta.push_str(&format!("  ({})", sha.chars().take(12).collect::<String>()));
        }
        lines.push(Line::from(vec![
            Span::styled(truncate(&meta, w), t.muted()),
            Span::styled(
                if info.is_gated() { "  · gated" } else { "" },
                Style::default().fg(t.warn),
            ),
        ]));
    }

    lines.push(app.hub_view.target.line(t, false, "—"));

    // Said here rather than at the keypress: a download button that fails after the fact
    // teaches nothing, and the fix is one key away.
    if app.hf_cli.is_none() {
        lines.push(Line::from(Span::styled(
            if app.hf_installing {
                "Installing the Hugging Face CLI…".to_string()
            } else {
                "The hf CLI is not installed — downloads need it. Press i to install.".to_string()
            },
            Style::default().fg(t.warn).add_modifier(Modifier::BOLD),
        )));
    }

    let (selected, _) = app.hub_view.selected();
    if selected > 0 {
        // Sampled on the hardware tick, not here: `statvfs` on network storage is not
        // something a render pass may wait on.
        let disk = app.disk_free_target.clone();
        let mut spans = vec![Span::styled(format!("{} to download", bytes(selected)), t.muted())];
        if let Some((measured, free_disk)) = disk {
            let color = if selected > free_disk { t.bad } else { t.dim };
            // Named, not "that filesystem": the figure is only meaningful with the path it
            // was taken from, and a wrong path is invisible without it.
            spans.push(Span::styled(
                format!("   {} free on {}", bytes(free_disk), measured.display()),
                Style::default().fg(color),
            ));
        }
        lines.push(Line::from(spans));
    } else if app.hub_view.compat.is_none() {
        lines.push(Line::from(Span::styled(
            if app.hub_view.layout.is_multi() {
                "Pick a quantization with Enter, then press d to download."
            } else {
                "Select files with space, then press d to download."
            },
            t.muted(),
        )));
    }

    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
}
