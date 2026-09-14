//! The Serve view: edit every `ft serve` knob, save the result as a profile, launch.
//!
//! Knobs are grouped rather than listed flat, because there are forty of them and the
//! groups match how the decisions actually cluster (what to load, where to bind, how to
//! spend VRAM, how to handle experts). Each row shows the effective value — either what
//! was set, or what FreeToken will do on its own — so an unset knob never reads as a
//! missing one.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use crate::knobs::{knobs_in, Group, Kind, Knob};
use crate::ui::app::App;
use crate::util::truncate;

pub fn render(f: &mut Frame, app: &mut App, area: Rect) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(24), Constraint::Min(30), Constraint::Length(34)])
        .split(area);

    groups(f, app, cols[0]);

    let mid = if app.serve_view.show_preview {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(6), Constraint::Length(8)])
            .split(cols[1])
    } else {
        Layout::default().constraints([Constraint::Min(1)]).split(cols[1])
    };
    knob_list(f, app, mid[0]);
    if app.serve_view.show_preview {
        preview(f, app, mid[1]);
    }

    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(10), Constraint::Min(6)])
        .split(cols[2]);
    help(f, app, right[0]);
    profiles(f, app, right[1]);
}

fn groups(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let block = t.pane("Groups", false);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let lines: Vec<Line> = Group::ALL
        .iter()
        .map(|g| {
            let active = *g == app.serve_view.group;
            let set = knobs_in(*g).filter(|k| app.serve.is_set(k.key)).count();
            let mut spans = vec![
                Span::styled(if active { "▌" } else { " " }, Style::default().fg(t.accent)),
                Span::styled(
                    format!("{:<16}", g.title()),
                    if active { t.selected() } else { t.text() },
                ),
            ];
            spans.push(Span::styled(
                if set > 0 { format!("{set:>3}") } else { "  ·".into() },
                if set > 0 { Style::default().fg(t.accent) } else { t.muted() },
            ));
            Line::from(spans)
        })
        .collect();

    f.render_widget(Paragraph::new(lines), inner);
}

fn knob_list(f: &mut Frame, app: &mut App, area: Rect) {
    let t = &app.theme;
    let group = app.serve_view.group;
    let items: Vec<&Knob> = knobs_in(group).collect();
    let errors = app.serve.validate();

    let block = t.pane(group.title(), !app.serve_view.in_profiles);
    let inner = block.inner(area);
    f.render_widget(block, area);

    app.serve_view.sel.clamp(items.len());
    let height = inner.height as usize;
    let range = app.serve_view.sel.window(items.len(), height);
    let selected = app.serve_view.sel.index;

    let label_w = 26usize;
    let value_w = inner.width.saturating_sub(label_w as u16 + 4) as usize;

    let mut lines: Vec<Line> = Vec::new();
    for i in range {
        let k = items[i];
        let is_sel = i == selected;
        let is_set = app.serve.is_set(k.key);
        let has_error = errors.iter().any(|(key, _)| key == k.key);

        // The selected row in edit mode shows the live editor instead of the value.
        if is_sel && app.serve_view.editing {
            let mut spans = vec![
                Span::styled("▌", Style::default().fg(t.accent)),
                Span::styled(format!("{:<label_w$}", truncate(k.label, label_w)), t.selected()),
            ];
            let editor = app.serve_view.editor.line(t, true, k.default);
            spans.extend(editor.spans);
            lines.push(Line::from(spans));
            continue;
        }

        let value = effective_value(app, k);
        let value_style = match (has_error, is_set, is_sel) {
            (true, _, _) => Style::default().fg(t.bad).add_modifier(Modifier::BOLD),
            (_, _, true) => t.selected(),
            (_, true, _) => Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
            (_, false, _) => t.muted(),
        };
        lines.push(Line::from(vec![
            Span::styled(if is_sel { "▌" } else { " " }, Style::default().fg(t.accent)),
            Span::styled(
                format!("{:<label_w$}", truncate(k.label, label_w)),
                if is_sel { t.selected() } else { t.text() },
            ),
            Span::styled(truncate(&value, value_w), value_style),
        ]));
    }

    f.render_widget(Paragraph::new(lines), inner);
}

/// What the knob will actually do: the set value, or the documented default in
/// parentheses so an unset row still says something concrete.
fn effective_value(app: &App, k: &Knob) -> String {
    match app.serve.get(k.key) {
        Some(v) => match k.kind {
            Kind::Flag => "on".to_string(),
            _ => v.to_string(),
        },
        None => format!("({})", k.default),
    }
}

fn preview(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let block = t.pane("Command", false);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let program = app.ft.as_ref().map(|f| f.display_program()).unwrap_or_else(|| "ft".into());
    let cmd = app.serve.preview(&program);

    let mut lines = vec![Line::from(Span::styled(cmd, t.text()))];
    let errors = app.serve.validate();
    if !errors.is_empty() {
        lines.push(Line::from(""));
        for (key, msg) in errors.iter().take(4) {
            let flag = crate::knobs::knob(key).map(|k| k.flag).unwrap_or(key);
            lines.push(Line::from(Span::styled(
                format!("{flag}: {msg}"),
                Style::default().fg(t.bad),
            )));
        }
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn help(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let items: Vec<&Knob> = knobs_in(app.serve_view.group).collect();
    let block = t.pane("What it does", false);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some(k) = items.get(app.serve_view.sel.index) else {
        f.render_widget(Paragraph::new("").style(t.muted()), inner);
        return;
    };

    let mut lines = vec![
        Line::from(Span::styled(
            k.flag,
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(k.help, t.text())),
        Line::from(""),
        Line::from(vec![Span::styled("default  ", t.label()), Span::styled(k.default, t.muted())]),
    ];

    if let Kind::Choice(options) | Kind::Multi(options) = k.kind {
        lines.push(Line::from(vec![
            Span::styled("options  ", t.label()),
            Span::styled(options.join(", "), t.muted()),
        ]));
        if matches!(k.kind, Kind::Multi(_)) {
            lines.push(Line::from(Span::styled(
                "         any number of them, separated by spaces",
                t.muted(),
            )));
        }
    }
    if !k.exclusive_with.is_empty() {
        let others: Vec<&str> = k
            .exclusive_with
            .iter()
            .filter(|o| **o != k.key)
            .filter_map(|o| crate::knobs::knob(o).map(|x| x.flag))
            .collect();
        if !others.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("excludes ", t.label()),
                Span::styled(others.join(", "), Style::default().fg(t.warn)),
            ]));
        }
    }

    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn profiles(f: &mut Frame, app: &mut App, area: Rect) {
    let t = &app.theme;
    let focused = app.serve_view.in_profiles;
    let block = t.pane(format!("Profiles ({})", app.profiles.items.len()), focused);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.serve_view.naming {
        let lines = vec![
            Line::from(Span::styled("Save this configuration as:", t.text())),
            Line::from(""),
            app.serve_view.profile_name.line(t, true, "profile name"),
            Line::from(""),
            Line::from(Span::styled("Enter to save, Esc to cancel", t.muted())),
        ];
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
        return;
    }

    if app.profiles.items.is_empty() {
        f.render_widget(
            Paragraph::new(
                "No saved profiles yet.\n\nPress S to save the current configuration under a name, \
                 then P to load it back.",
            )
            .style(t.muted())
            .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    }

    let height = inner.height as usize;
    app.serve_view.profile_sel.clamp(app.profiles.items.len());
    let range = app.serve_view.profile_sel.window(app.profiles.items.len(), height);
    let selected = app.serve_view.profile_sel.index;
    let w = inner.width.saturating_sub(2) as usize;

    let mut lines: Vec<Line> = Vec::new();
    for i in range {
        let p = &app.profiles.items[i];
        let is_sel = i == selected && focused;
        let is_current = app.profiles.last_used.as_deref() == Some(p.name.as_str());
        lines.push(Line::from(vec![
            Span::styled(if is_sel { "▌" } else { " " }, Style::default().fg(t.accent)),
            Span::styled(
                truncate(&p.name, w.saturating_sub(10)),
                if is_sel { t.selected() } else { t.text() },
            ),
            Span::styled(if is_current { "  active" } else { "" }, Style::default().fg(t.good)),
        ]));
        let model = p.serve.get("model").unwrap_or("—");
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(truncate(model, w.saturating_sub(2)), t.muted()),
        ]));
    }

    f.render_widget(Paragraph::new(lines), inner);
}
