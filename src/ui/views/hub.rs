//! The Hub view: search Hugging Face, pick files, download.
//!
//! Search results on the left; the selected repo's file list on the right, pre-filtered
//! to what a serving engine actually reads. Every file's selection is editable, because
//! the default heuristic cannot know that a particular repo ships three quantizations
//! and only one of them is wanted.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use crate::ui::app::App;
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

    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(6), Constraint::Length(4)])
        .split(cols[1]);
    files(f, app, right[0]);
    target(f, app, right[1]);
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
    let focused = !app.hub_view.in_files && !app.hub_view.editing;
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
    let focused = app.hub_view.in_files;

    let (selected_bytes, selected_count) = app
        .hub_view
        .files
        .iter()
        .filter(|x| x.wanted)
        .fold((0u64, 0usize), |(b, c), x| (b + x.size, c + 1));

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
    let block = t.pane("Download to", false);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines = vec![app.hub_view.target.line(t, false, "—")];
    if let Some(info) = &app.hub_view.info {
        let mut meta = format!("{} @ {}", info.id, app.hub_view.revision);
        if let Some(sha) = &info.sha {
            meta.push_str(&format!("  ({})", sha.chars().take(12).collect::<String>()));
        }
        if info.is_gated() {
            meta.push_str("  · gated");
        }
        lines.push(Line::from(Span::styled(meta, t.muted())));
    }
    let selected: u64 = app.hub_view.files.iter().filter(|x| x.wanted).map(|x| x.size).sum();
    if selected > 0 {
        let disk = disk_free(&app.hub_view.target.value);
        let mut spans = vec![Span::styled(format!("{} to download", bytes(selected)), t.muted())];
        if let Some(free_disk) = disk {
            let color = if selected > free_disk { t.bad } else { t.dim };
            spans.push(Span::styled(
                format!("   {} free on that filesystem", bytes(free_disk)),
                Style::default().fg(color),
            ));
        }
        lines.push(Line::from(spans));
    } else {
        lines.push(Line::from(Span::styled(
            "Select files with space, then press d to download.",
            t.muted(),
        )));
    }

    f.render_widget(Paragraph::new(lines), inner);
}

/// Free bytes on the filesystem holding `path`, walking up to the nearest existing
/// ancestor so a not-yet-created target directory still reports something useful.
fn disk_free(path: &str) -> Option<u64> {
    let mut p = std::path::Path::new(path);
    loop {
        if p.exists() {
            break;
        }
        p = p.parent()?;
    }
    let c = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    (unsafe { libc::statvfs(c.as_ptr(), &mut stat) } == 0)
        .then(|| stat.f_bavail as u64 * stat.f_frsize as u64)
}
