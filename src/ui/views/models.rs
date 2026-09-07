//! The Models view: the local checkpoint library.
//!
//! Two columns — the list on the left, full detail on the right — because the decisions
//! made here ("is this already converted?", "will it fit?", "what quantization is it?")
//! all need more than a table row can carry.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use crate::models::{Format, Model};
use crate::ui::app::App;
use crate::util::{bytes, count, truncate, truncate_left};

pub fn render(f: &mut Frame, app: &mut App, area: Rect) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(56), Constraint::Percentage(44)])
        .split(area);

    let list_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(3)])
        .split(cols[0]);

    list(f, app, list_rows[0]);
    filter_bar(f, app, list_rows[1]);
    detail(f, app, cols[1]);
}

fn list(f: &mut Frame, app: &mut App, area: Rect) {
    // `Theme` is `Copy`, so taking it by value releases the borrow on `app` before the
    // selection state below needs it mutably.
    let t = app.theme;
    let total = app.models.len();
    let count = app.filtered_models().len();
    let title = if app.models_view.scanning {
        "Library (scanning…)".to_string()
    } else if app.models_view.filter.is_empty() {
        format!("Library ({count})")
    } else {
        format!("Library ({count} of {total})")
    };
    let block = t.pane(title, !app.models_view.filtering);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if count == 0 {
        let msg = if app.models_view.scanning {
            "Scanning…".to_string()
        } else if app.models.is_empty() {
            // Each root is marked present or missing. An empty library and a library
            // pointed at a directory that does not exist look identical otherwise, and
            // they need completely different fixes.
            let listed: Vec<String> = app
                .config
                .library
                .effective_roots()
                .iter()
                .map(|r| {
                    let missing = if r.is_dir() { "" } else { "   (does not exist)" };
                    format!("  {}{missing}", r.display())
                })
                .collect();
            format!(
                "No checkpoints found under:\n{}\n\nDownload one from the Hub tab, or set \
                 library.roots / library.hub_cache in {}.",
                listed.join("\n"),
                crate::config::config_path().display()
            )
        } else {
            "No model matches the filter.".to_string()
        };
        f.render_widget(Paragraph::new(msg).style(t.muted()).wrap(Wrap { trim: false }), inner);
        return;
    }

    // Two rows per entry: the name line and the summary line beneath it.
    let height = ((inner.height as usize) / 2).max(1);
    app.models_view.sel.clamp(count);
    let range = app.models_view.sel.window(count, height);
    let selected = app.models_view.sel.index;
    let models = app.filtered_models();

    let name_w = inner.width.saturating_sub(22) as usize;
    let mut lines: Vec<Line> = Vec::with_capacity(range.len());
    for i in range {
        let m = models[i];
        let is_sel = i == selected;
        let base = if is_sel { t.selected() } else { t.text() };
        let fmt_color = match m.format {
            Format::Ftw => t.good,
            Format::Hf => t.accent,
            Format::Gguf => t.warn,
            Format::PartialFtw => t.bad,
        };
        let marker = if m.converted_to.is_some() { "→" } else { " " };
        lines.push(Line::from(vec![
            Span::styled(if is_sel { "▌" } else { " " }, Style::default().fg(t.accent)),
            Span::styled(
                format!("{:<5}", m.format.label()),
                Style::default().fg(fmt_color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("{:<name_w$}", truncate(&m.name, name_w)), base),
            Span::styled(marker, Style::default().fg(t.good)),
            Span::styled(
                format!("{:>10}", bytes(m.size_bytes)),
                if is_sel { base } else { t.muted() },
            ),
        ]));
        // A second, dimmer line carries the shape of the checkpoint, which is what the
        // name alone usually fails to say.
        let summary = m.summary();
        if !summary.is_empty() {
            lines.push(Line::from(vec![
                Span::raw("      "),
                Span::styled(truncate(&summary, name_w), t.muted()),
            ]));
        }
    }

    f.render_widget(Paragraph::new(lines), inner);
}

fn filter_bar(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let block = t.pane("Filter", app.models_view.filtering);
    let inner = block.inner(area);
    f.render_widget(block, area);
    f.render_widget(
        Paragraph::new(app.models_view.filter.line(
            t,
            app.models_view.filtering,
            "press / to filter by name, path or architecture",
        )),
        inner,
    );
}

fn detail(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let block = t.pane("Details", false);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some(m) = app.selected_model() else {
        f.render_widget(
            Paragraph::new("Select a model to see its details.").style(t.muted()),
            inner,
        );
        return;
    };

    let w = inner.width.saturating_sub(1) as usize;
    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            truncate(&m.name, w),
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(truncate_left(&m.path.display().to_string(), w), t.muted())),
        Line::from(""),
        t.field("Format", format_description(m)),
    ];
    lines.push(t.field("On disk", bytes(m.size_bytes)));
    if let Some(when) = m.modified {
        let stamp: chrono::DateTime<chrono::Local> = when.into();
        lines.push(t.field("Modified", stamp.format("%Y-%m-%d %H:%M").to_string()));
    }
    if let Some(a) = &m.arch {
        lines.push(t.field("Architecture", a.clone()));
    }
    if let Some(mt) = &m.model_type {
        lines.push(t.field("Model type", mt.clone()));
    }
    if let Some(q) = &m.quant {
        lines.push(t.field("Quantization", q.to_uppercase()));
    }
    if let Some(l) = m.num_layers {
        lines.push(t.field("Layers", count(l)));
    }
    if m.is_moe {
        lines.push(t.field_colored(
            "Experts",
            m.num_experts.map(count).unwrap_or_else(|| "yes (count unknown)".into()),
            t.accent,
        ));
    }
    if let Some(ctx) = m.max_position {
        lines.push(t.field("Max context", format!("{} tokens", count(ctx))));
    }
    if let Some(fp) = &m.ftw_fingerprint {
        lines.push(t.field("Fingerprint", truncate(fp, w.saturating_sub(18))));
    }
    let template = app.template_status(m);
    match &template {
        crate::templates::Status::BuiltIn => lines.push(t.field("Chat template", "built-in")),
        crate::templates::Status::Foreign => lines.push(t.field_colored(
            "Chat template",
            "custom file, not applied by ft-man",
            t.warn,
        )),
        crate::templates::Status::Overridden(_) => lines.push(t.field_colored(
            "Chat template",
            truncate(&template.label(), w.saturating_sub(18)),
            t.accent,
        )),
    }

    if let Some(dest) = &m.converted_to {
        lines.push(Line::from(""));
        lines.push(t.field_colored(
            "Converted",
            truncate_left(&dest.display().to_string(), w.saturating_sub(18)),
            t.good,
        ));
    }

    lines.push(Line::from(""));
    lines.extend(guidance(app, m, w));

    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn format_description(m: &Model) -> String {
    match m.format {
        Format::Ftw => "FTW — FreeToken fast-load".into(),
        Format::Hf => "Hugging Face safetensors".into(),
        Format::Gguf => "GGUF".into(),
        Format::PartialFtw => "incomplete FTW conversion".into(),
    }
}

/// What to do with this checkpoint next, stated plainly. The rules come from
/// FreeToken's own docs: conversion is optional, offload is what MoE models get by
/// default, and the expert banks live in host RAM.
fn guidance<'a>(app: &App, m: &Model, w: usize) -> Vec<Line<'a>> {
    let t = &app.theme;
    let mut out: Vec<Line> = Vec::new();
    let mut note = |text: String, color: ratatui::style::Color| {
        out.push(Line::from(Span::styled(
            truncate(&format!("• {text}"), w),
            Style::default().fg(color),
        )));
    };

    if m.is_partial() {
        note(
            "A conversion died before writing its index, so these shards are unusable. \
             Delete it with D to reclaim the space and free the name for a retry."
                .into(),
            t.bad,
        );
        return out;
    }

    if m.converted_to.is_some() {
        note("An FTW build already exists; serving that one loads faster.".into(), t.good);
    } else if m.convertible() {
        note("Serves as-is. Converting to FTW (c) speeds up every later load.".into(), t.dim);
    } else if m.format == Format::Ftw {
        note("Ready to serve directly — FTW is auto-detected by --model.".into(), t.good);
    } else {
        note("Served natively; no conversion step applies.".into(), t.dim);
    }

    if m.is_moe {
        let host_free = app.host.memory_free();
        if host_free > 0 && m.size_bytes > host_free {
            note(
                format!(
                    "Expert banks need host RAM: {} on disk vs {} free.",
                    bytes(m.size_bytes),
                    bytes(host_free)
                ),
                t.warn,
            );
        }
        if let Some(gpu) = app.gpus.first() {
            if m.size_bytes > gpu.memory_total {
                note(
                    format!(
                        "Larger than {} of VRAM — an offload MoE backend is required.",
                        bytes(gpu.memory_total)
                    ),
                    t.dim,
                );
            }
        }
    } else if let Some(gpu) = app.gpus.first() {
        if m.size_bytes > gpu.memory_total {
            note(
                format!(
                    "Dense model larger than {} of VRAM; it may not load.",
                    bytes(gpu.memory_total)
                ),
                t.warn,
            );
        }
    }

    if m.name.to_lowercase().contains("deepseek") && !m.path.join("inference/config.json").is_file()
    {
        note(
            "DeepSeek-V4 checkpoints need their inference/config.json subdirectory.".into(),
            t.warn,
        );
    }

    out
}
