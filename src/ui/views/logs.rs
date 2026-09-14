//! The Logs view: the engine's stdout and stderr.
//!
//! FreeToken logs a lot — scheduler status lines, kernel JIT chatter, load progress — so
//! the useful features here are a filter, an errors-only toggle, and a tail that can be
//! detached to scroll back without new lines yanking the viewport.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use crate::ui::app::App;

pub fn render(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(4), Constraint::Length(3)])
        .split(area);

    body(f, app, rows[0]);
    filter_bar(f, app, rows[1]);
}

fn body(f: &mut Frame, app: &mut App, area: Rect) {
    let t = &app.theme;
    let all = app.engine.log.snapshot();
    let needle = app.logs_view.filter.value.to_lowercase();
    let lines: Vec<&crate::ft::proc::LogLine> = all
        .iter()
        .filter(|l| !app.logs_view.errors_only || l.err || is_error_text(&l.text))
        .filter(|l| needle.is_empty() || l.text.to_lowercase().contains(&needle))
        .collect();

    let mut title = format!("Engine log ({} lines)", lines.len());
    if app.logs_view.errors_only {
        title.push_str(" — errors only");
    }
    if !app.logs_view.follow {
        title.push_str(" — paused");
    }
    let block = t.pane(title, !app.logs_view.filtering);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if lines.is_empty() {
        let msg = if all.is_empty() {
            match app.engine.log_path.as_ref() {
                Some(p) => format!("No output yet.\n\nThe engine's log file is {}.", p.display()),
                None => {
                    "No engine has been started from paddock in this session.\n\nStart one from \
                         the Serve tab (Enter), or attach to a server that is already running by \
                         pointing server.host and server.port at it."
                        .to_string()
                }
            }
        } else {
            "No lines match the current filter.".to_string()
        };
        f.render_widget(Paragraph::new(msg).style(t.muted()).wrap(Wrap { trim: true }), inner);
        return;
    }

    let height = inner.height as usize;
    // `scroll` counts lines back from the newest, which keeps following the tail as the
    // trivial case (scroll == 0) rather than something to recompute per frame.
    let max_scroll = lines.len().saturating_sub(height);
    app.logs_view.scroll = app.logs_view.scroll.min(max_scroll);
    if app.logs_view.follow {
        app.logs_view.scroll = 0;
    }
    let end = lines.len() - app.logs_view.scroll;
    let start = end.saturating_sub(height);

    let width = inner.width as usize;
    let rendered: Vec<Line> = lines[start..end]
        .iter()
        .map(|l| {
            let style = line_style(app, &l.text, l.err);
            let text = if app.logs_view.wrap {
                l.text.clone()
            } else {
                crate::util::truncate(&l.text, width)
            };
            Line::from(Span::styled(text, style))
        })
        .collect();

    let paragraph = if app.logs_view.wrap {
        Paragraph::new(rendered).wrap(Wrap { trim: false })
    } else {
        Paragraph::new(rendered)
    };
    f.render_widget(paragraph, inner);
}

/// How a log line reads, picked out of the line rather than from the stream it arrived on
/// — Python's logging writes everything to stderr, so the stream alone says nothing.
///
/// One classifier, shared: the terminal turns this into a theme color and the web API
/// sends the name on every `LogLine`, so a browser never re-derives a rule that could
/// drift from the one the terminal applies to the same text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warn,
    /// A line paddock wrote itself, tagged `[paddock]`.
    Meta,
    Normal,
}

/// Classify one line of engine output. `err` is whether it arrived on stderr.
pub fn classify(text: &str, err: bool) -> Severity {
    if text.starts_with("[paddock]") {
        return Severity::Meta;
    }
    if is_error_text(text) {
        return Severity::Error;
    }
    if text.contains("WARNING") || text.contains("WARN") {
        return Severity::Warn;
    }
    // `err` deliberately decides nothing: FreeToken logs its whole life to stderr, and
    // coloring on the stream would paint every informational line as a problem.
    let _ = err;
    Severity::Normal
}

fn line_style(app: &App, text: &str, err: bool) -> Style {
    let t = &app.theme;
    match classify(text, err) {
        Severity::Meta => Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
        Severity::Error => Style::default().fg(t.bad),
        Severity::Warn => Style::default().fg(t.warn),
        Severity::Normal if text.contains("ready to serve") => {
            Style::default().fg(t.good).add_modifier(Modifier::BOLD)
        }
        Severity::Normal => t.text(),
    }
}

/// Whether a line names a failure. Also the errors-only filter's rule, which is why it is
/// separate from [`classify`].
pub fn is_error_text(text: &str) -> bool {
    text.contains("ERROR")
        || text.contains("CRITICAL")
        || text.contains("Traceback")
        || text.contains("Exception")
}

fn filter_bar(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let block = t.pane("Filter", app.logs_view.filtering);
    let inner = block.inner(area);
    f.render_widget(block, area);
    f.render_widget(
        Paragraph::new(app.logs_view.filter.line(
            t,
            app.logs_view.filtering,
            "press / to filter, e f for errors only, w to wrap",
        )),
        inner,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_detection_covers_python_shapes() {
        assert!(is_error_text("ERROR:freetoken.engine:boom"));
        assert!(is_error_text("Traceback (most recent call last):"));
        assert!(is_error_text("torch.cuda.OutOfMemoryError: Exception"));
        assert!(!is_error_text("INFO: loading weights"));
    }

    /// The web API sends this name on every line, so the mapping is asserted rather than
    /// left to whichever front end happens to be read first.
    #[test]
    fn classification_names_the_same_four_cases_the_terminal_colors() {
        assert_eq!(classify("[paddock] $ ft serve --model x", false), Severity::Meta);
        // Our own lines win over their content: the exit line names a status, not an error.
        assert_eq!(classify("[paddock] engine exited: ERROR", true), Severity::Meta);
        assert_eq!(classify("ERROR:freetoken.engine:boom", false), Severity::Error);
        assert_eq!(classify("WARNING: falling back to torch", false), Severity::Warn);
        assert_eq!(classify("INFO: loading weights", false), Severity::Normal);
        // stderr alone never decides: FreeToken logs everything there.
        assert_eq!(classify("INFO: loading weights", true), Severity::Normal);
    }

    #[test]
    fn the_severity_names_are_the_wire_spelling() {
        for (severity, name) in [
            (Severity::Error, "error"),
            (Severity::Warn, "warn"),
            (Severity::Meta, "meta"),
            (Severity::Normal, "normal"),
        ] {
            assert_eq!(serde_json::to_value(severity).unwrap(), serde_json::json!(name));
        }
    }
}
