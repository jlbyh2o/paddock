//! The help overlay: every binding, grouped by where it applies.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::ui::app::App;
use crate::ui::widgets::modal;

/// `(section, [(keys, description)])`
const SECTIONS: &[(&str, &[(&str, &str)])] = &[
    (
        "Global",
        &[
            ("1-8 / Tab", "switch view"),
            ("? or F1", "this help"),
            ("q", "quit (asks first if the engine is running)"),
            ("Ctrl-C", "quit immediately"),
            ("Esc", "close an overlay, or cancel an edit"),
        ],
    ),
    (
        "Dashboard",
        &[
            ("e", "start the engine with the current Serve configuration"),
            ("s", "stop the engine"),
            ("S", "force-stop the engine (SIGKILL)"),
            ("t", "run a /generate smoke test"),
            ("r", "rescan the model library"),
        ],
    ),
    (
        "Models",
        &[
            ("↑ ↓ / j k", "move"),
            ("/", "filter; Esc clears"),
            ("Enter", "load into the Serve configuration"),
            ("c", "convert to FTW"),
            ("s", "serve this model now"),
            ("D", "delete the checkpoint from disk"),
            ("r", "rescan"),
        ],
    ),
    (
        "Hub",
        &[
            ("/", "search"),
            ("Enter", "list a repo's files"),
            ("Tab", "move between results and files"),
            ("Space", "toggle a file"),
            ("a / n", "select all / none"),
            ("d", "download the selected files"),
        ],
    ),
    (
        "Serve",
        &[
            ("↑ ↓", "move between knobs"),
            ("← →", "switch knob group"),
            ("Enter", "edit a value, or toggle a flag"),
            ("Space", "cycle a choice knob"),
            ("x / Del", "unset a knob, back to its default"),
            ("p", "show the resolved command line"),
            ("Tab", "move to the profile list"),
            ("S", "save as a profile"),
            ("P", "load the selected profile"),
            ("D", "delete the selected profile"),
            ("Ctrl-Enter or g", "start the engine"),
        ],
    ),
    (
        "Cache",
        &[
            ("↑ ↓", "select a pool"),
            ("← →", "adjust by 1%"),
            ("Shift + ← →", "adjust by 10%"),
            ("r", "reset the selected pool"),
            ("R", "reset every pending change"),
            ("a", "apply the rebuild"),
        ],
    ),
    (
        "Jobs",
        &[
            ("↑ ↓", "select"),
            ("b", "run ft bench bw"),
            ("Tab", "focus the output pane"),
            ("x", "cancel the selected job"),
            ("X", "clear finished entries"),
        ],
    ),
    (
        "Requests",
        &[
            ("↑ ↓", "move"),
            ("Enter", "toggle the detail pane"),
            ("f", "follow the newest entry"),
            ("p", "pause polling"),
            ("c", "clear"),
        ],
    ),
    (
        "Logs",
        &[
            ("↑ ↓ / PgUp PgDn", "scroll"),
            ("G / End", "jump to the tail and follow"),
            ("f", "toggle follow"),
            ("/", "filter"),
            ("e", "errors only"),
            ("w", "wrap long lines"),
            ("c", "clear the buffer"),
        ],
    ),
];

pub fn render(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let inner = modal(f, t, area, "Keys", 96, 34);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(inner);

    // Split so the two columns end up roughly the same height rather than the left one
    // holding everything.
    let total: usize = SECTIONS.iter().map(|(_, k)| k.len() + 2).sum();
    let mut running = 0usize;
    let mut split_at = SECTIONS.len();
    for (i, (_, keys)) in SECTIONS.iter().enumerate() {
        running += keys.len() + 2;
        if running * 2 >= total {
            split_at = i + 1;
            break;
        }
    }

    f.render_widget(Paragraph::new(column(app, &SECTIONS[..split_at])), cols[0]);
    f.render_widget(Paragraph::new(column(app, &SECTIONS[split_at..])), cols[1]);
}

fn column<'a>(app: &App, sections: &'a [(&'a str, &'a [(&'a str, &'a str)])]) -> Vec<Line<'a>> {
    let t = &app.theme;
    let mut lines = Vec::new();
    for (title, keys) in sections {
        lines.push(Line::from(Span::styled(
            *title,
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
        )));
        for (key, what) in *keys {
            lines.push(Line::from(vec![
                Span::styled(format!("  {key:<18}"), t.value()),
                Span::styled(*what, t.muted()),
            ]));
        }
        lines.push(Line::from(""));
    }
    lines
}
