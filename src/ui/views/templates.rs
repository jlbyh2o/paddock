//! The Templates view: fetch chat templates and apply them to checkpoints.
//!
//! FreeToken has no chat-template flag — it reads the template out of the checkpoint
//! directory — so applying one here writes `chat_template.jinja` into that directory.
//! The view is built to keep that honest: it says which model would be written to, names
//! every directory that will change, and shows whether a checkpoint is currently running
//! its own template or an override.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use crate::templates::Status;
use crate::ui::app::{App, TemplatePane};
use crate::util::{bytes, truncate, truncate_left};

pub fn render(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(8), Constraint::Length(7)])
        .split(area);

    repo_bar(f, app, rows[0]);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(38),
            Constraint::Percentage(28),
            Constraint::Percentage(34),
        ])
        .split(rows[1]);

    store(f, app, cols[0]);
    remote(f, app, cols[1]);
    preview(f, app, cols[2]);

    target(f, app, rows[2]);
}

fn repo_bar(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let title =
        if app.templates_view.loading { "Template repo (loading…)" } else { "Template repo" };
    let block = t.pane(title, app.templates_view.editing_repo);
    let inner = block.inner(area);
    f.render_widget(block, area);
    f.render_widget(
        Paragraph::new(app.templates_view.repo.line(
            t,
            app.templates_view.editing_repo,
            "press r to enter a Hugging Face repo, then Enter to list its .jinja files",
        )),
        inner,
    );
}

fn store(f: &mut Frame, app: &mut App, area: Rect) {
    let t = app.theme;
    let focused =
        app.templates_view.pane == TemplatePane::Store && !app.templates_view.editing_repo;
    let count = app.templates_view.stored.len();
    let block = t.pane(format!("Stored templates ({count})"), focused);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if count == 0 {
        f.render_widget(
            Paragraph::new(
                "No templates yet.\n\nEnter a repo above and press Enter to list its templates, \
                 then f to fetch one.\n\nAnything holding .jinja files works.",
            )
            .style(t.muted())
            .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    }

    // Two rows per entry: name, then version and source.
    let height = ((inner.height as usize) / 2).max(1);
    app.templates_view.sel.clamp(count);
    let range = app.templates_view.sel.window(count, height);
    let selected = app.templates_view.sel.index;
    let w = inner.width.saturating_sub(3) as usize;

    let mut lines: Vec<Line> = Vec::new();
    for i in range {
        let tpl = &app.templates_view.stored[i];
        let is_sel = i == selected;
        lines.push(Line::from(vec![
            Span::styled(if is_sel { "▌" } else { " " }, Style::default().fg(t.accent)),
            Span::styled(
                truncate(&tpl.name, w.saturating_sub(9)),
                if is_sel { t.selected() } else { t.text() },
            ),
            Span::styled(format!("  {}", bytes(tpl.size)), t.muted()),
        ]));
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(truncate(&tpl.subtitle(), w.saturating_sub(2)), t.muted()),
        ]));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn remote(f: &mut Frame, app: &mut App, area: Rect) {
    let t = app.theme;
    let focused =
        app.templates_view.pane == TemplatePane::Remote && !app.templates_view.editing_repo;
    let title = match &app.templates_view.remote_repo {
        Some(r) => format!(
            "In {} ({})",
            r.rsplit('/').next().unwrap_or(r),
            app.templates_view.remote.len()
        ),
        None => "Repo contents".to_string(),
    };
    let block = t.pane(title, focused);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.templates_view.remote.is_empty() {
        let msg = if app.templates_view.loading {
            "Listing…"
        } else {
            "Enter a repo above and press Enter to see the templates it holds."
        };
        f.render_widget(Paragraph::new(msg).style(t.muted()).wrap(Wrap { trim: true }), inner);
        return;
    }

    let count = app.templates_view.remote.len();
    let height = inner.height as usize;
    app.templates_view.remote_sel.clamp(count);
    let range = app.templates_view.remote_sel.window(count, height);
    let selected = app.templates_view.remote_sel.index;
    let w = inner.width.saturating_sub(2) as usize;
    let have: Vec<String> = app.templates_view.stored.iter().map(|t| t.name.clone()).collect();
    let repo = app.templates_view.remote_repo.clone().unwrap_or_default();

    let mut lines: Vec<Line> = Vec::new();
    for i in range {
        let file = &app.templates_view.remote[i];
        let is_sel = i == selected && focused;
        let already = have.contains(&crate::templates::name_for(&repo, &file.path));
        lines.push(Line::from(vec![
            Span::styled(if is_sel { "▌" } else { " " }, Style::default().fg(t.accent)),
            Span::styled(
                truncate_left(&file.path, w.saturating_sub(2)),
                if is_sel { t.selected() } else { t.text() },
            ),
            Span::styled(if already { " ✓" } else { "" }, Style::default().fg(t.good)),
        ]));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn preview(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let block = t.pane("Preview", false);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some(tpl) = app.selected_template() else {
        f.render_widget(Paragraph::new("").style(t.muted()), inner);
        return;
    };

    let mut lines: Vec<Line> = Vec::new();
    if let Some(v) = &tpl.meta.version {
        lines.push(t.field("Version", v.clone()));
    }
    if let Some(s) = &tpl.meta.source {
        lines.push(t.field("Source", s.clone()));
    }
    if let Some(p) = &tpl.meta.repo_path {
        lines.push(t.field("Path", p.clone()));
    }
    if let Some(rev) = &tpl.meta.revision {
        lines.push(t.field("Revision", rev.chars().take(12).collect::<String>()));
    }
    if let Some(at) = &tpl.meta.fetched_at {
        lines.push(t.field("Fetched", at.clone()));
    }

    // The last render check, when it was for this template.
    if let Some((name, outcome)) = &app.templates_view.preflight {
        if *name == tpl.name {
            use crate::templates::Preflight;
            let color = match outcome {
                Preflight::Ok(_) => t.good,
                Preflight::Warn(_) => t.warn,
                Preflight::Fail(_) => t.bad,
            };
            let width = inner.width.saturating_sub(18) as usize;
            lines.push(t.field_colored("Render check", truncate(outcome.detail(), width), color));
        }
    } else if app.templates_view.checking {
        lines.push(t.field("Render check", "running…"));
    }

    lines.push(Line::from(""));
    if let Some((_, text)) = &app.templates_view.preview {
        let room = inner.height.saturating_sub(lines.len() as u16) as usize;
        let w = inner.width as usize;
        lines.extend(
            text.lines().take(room).map(|l| Line::from(Span::styled(truncate(l, w), t.muted()))),
        );
    }
    f.render_widget(Paragraph::new(lines), inner);
}

/// What applying would do, to which directories. This pane exists because the operation
/// modifies a checkpoint on disk, and that should never be a surprise.
fn target(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let block = t.pane("Apply to", false);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some(model) = app.selected_model() else {
        f.render_widget(
            Paragraph::new(
                "No model selected. Pick one on the Models tab; applying writes \
                 chat_template.jinja into that checkpoint's directory, because FreeToken \
                 reads the template from there rather than from a command-line flag.",
            )
            .style(t.muted())
            .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    };

    let w = inner.width.saturating_sub(20) as usize;
    let status = app.template_status(model);
    let mut lines = vec![
        t.field("Model", truncate(&model.name, w)),
        match &status {
            Status::BuiltIn => t.field("Currently", "built-in template"),
            Status::Foreign => {
                t.field_colored("Currently", "a chat_template.jinja ft-man did not write", t.warn)
            }
            Status::Overridden(_) => t.field_colored("Currently", status.label(), t.accent),
        },
    ];

    let targets = crate::templates::targets(model);
    let list = targets
        .iter()
        .map(|p| truncate_left(&p.display().to_string(), w))
        .collect::<Vec<_>>()
        .join("   ");
    lines.push(t.field(if targets.len() > 1 { "Writes into (2)" } else { "Writes into" }, list));

    if targets.len() > 1 {
        lines.push(Line::from(Span::styled(
            "The checkpoint and its FTW build are the same model and each carries its own \
             tokenizer files, so both are written — otherwise the override would silently \
             not apply to whichever one you serve.",
            t.muted(),
        )));
    }

    // A template that is in place but does not render breaks every request the engine
    // serves, so say plainly how to back out of it.
    let broken =
        app.templates_view.preflight.as_ref().is_some_and(|(_, outcome)| outcome.is_fail());
    if broken && status.is_overridden() {
        lines.push(Line::from(Span::styled(
            "The applied template failed its render check — press u to restore the \
             checkpoint's own template.",
            Style::default().fg(t.bad),
        )));
    }

    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
}
