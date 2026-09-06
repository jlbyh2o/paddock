//! The outer chrome: tab bar, content area, footer hints, toasts and overlays.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::ui::app::{App, Tab};
use crate::ui::views;
use crate::ui::widgets::{footer, ToastKind};
use crate::util::truncate;

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(4), Constraint::Length(2)])
        .split(area);

    tab_bar(f, app, rows[0]);
    content(f, app, rows[1]);
    footer(f, &app.theme, rows[2], hints(app));

    toasts(f, app, rows[1]);

    if let Some(confirm) = app.confirm.clone() {
        confirm.render(f, &app.theme, area);
    }
    if app.show_help {
        views::help::render(f, app, area);
    }
}

fn tab_bar(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let mut spans: Vec<Span> = vec![Span::styled(" ft-man ", t.title())];

    for (i, tab) in Tab::ALL.iter().enumerate() {
        let active = *tab == app.tab;
        let mut label = format!(" {} {} ", i + 1, tab.title());
        // Badge the tabs that have something happening, so a background conversion or a
        // failing engine is visible from any screen.
        let badge = match tab {
            Tab::Jobs => {
                let n = app.active_jobs() + app.active_downloads();
                (n > 0).then(|| format!("{n}"))
            }
            Tab::Models => (!app.models.is_empty()).then(|| app.models.len().to_string()),
            Tab::Templates => (!app.templates_view.stored.is_empty())
                .then(|| app.templates_view.stored.len().to_string()),
            _ => None,
        };
        if let Some(b) = badge {
            label = format!("{}·{b} ", label.trim_end());
        }
        spans.push(Span::styled(
            label,
            if active {
                Style::default().fg(t.selection_fg).bg(t.accent).add_modifier(Modifier::BOLD)
            } else {
                t.muted()
            },
        ));
    }

    let left = Paragraph::new(Line::from(spans));
    f.render_widget(left, area);

    // Right-aligned status: engine state and endpoint, always in the same place.
    let right_text = status_summary(app);
    let width = right_text.chars().count() as u16;
    if area.width > width + 4 {
        let rect =
            Rect { x: area.x + area.width - width - 1, y: area.y, width: width + 1, height: 1 };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("● ", Style::default().fg(app.engine_status_color())),
                Span::styled(right_text.trim_start_matches("● ").to_string(), t.muted()),
            ]))
            .alignment(Alignment::Right),
            rect,
        );
    }
}

fn status_summary(app: &App) -> String {
    let model = app.current_model().map(|m| truncate(&m, 28)).unwrap_or_else(|| "no model".into());
    format!("● {} · {}", app.engine_status_text(), model)
}

fn content(f: &mut Frame, app: &mut App, area: Rect) {
    match app.tab {
        Tab::Dashboard => views::dashboard::render(f, app, area),
        Tab::Models => views::models::render(f, app, area),
        Tab::Hub => views::hub::render(f, app, area),
        Tab::Templates => views::templates::render(f, app, area),
        Tab::Serve => views::serve::render(f, app, area),
        Tab::Cache => views::cache::render(f, app, area),
        Tab::Jobs => views::jobs::render(f, app, area),
        Tab::Requests => views::requests::render(f, app, area),
        Tab::Logs => views::logs::render(f, app, area),
    }
}

/// Context-sensitive footer hints. Only the bindings that apply right now, so the line
/// stays short enough to read.
fn hints(app: &App) -> &'static [(&'static str, &'static str)] {
    if app.confirm.is_some() {
        return &[("← →", "choose"), ("Enter", "accept"), ("Esc", "cancel")];
    }
    if app.show_help {
        return &[("Esc", "close")];
    }
    match app.tab {
        Tab::Dashboard => {
            &[("e", "start"), ("s", "stop"), ("t", "smoke test"), ("r", "rescan"), ("?", "keys")]
        }
        Tab::Models => &[
            ("Enter", "use"),
            ("s", "serve"),
            ("c", "convert"),
            ("/", "filter"),
            ("D", "delete"),
            ("?", "keys"),
        ],
        Tab::Hub => &[
            ("/", "search"),
            ("Enter", "files"),
            ("Space", "toggle"),
            ("d", "download"),
            ("?", "keys"),
        ],
        Tab::Templates => &[
            ("r", "repo"),
            ("Enter", "list"),
            ("f", "fetch"),
            ("a", "apply"),
            ("u", "revert"),
            ("v", "verify"),
            ("?", "keys"),
        ],
        Tab::Serve => &[
            ("Enter", "edit"),
            ("← →", "group"),
            ("x", "unset"),
            ("g", "start engine"),
            ("S", "save profile"),
            ("?", "keys"),
        ],
        Tab::Cache => &[("← →", "adjust"), ("r", "reset"), ("a", "apply"), ("?", "keys")],
        Tab::Jobs => &[
            ("b", "bench"),
            ("x", "cancel"),
            ("X", "clear done"),
            ("Tab", "output"),
            ("?", "keys"),
        ],
        Tab::Requests => {
            &[("Enter", "detail"), ("f", "follow"), ("p", "pause"), ("c", "clear"), ("?", "keys")]
        }
        Tab::Logs => &[
            ("/", "filter"),
            ("e", "errors"),
            ("w", "wrap"),
            ("G", "tail"),
            ("c", "clear"),
            ("?", "keys"),
        ],
    }
}

/// Toasts stack in the bottom-right of the content area, newest last.
fn toasts(f: &mut Frame, app: &App, area: Rect) {
    if app.toasts.is_empty() {
        return;
    }
    let t = &app.theme;
    let max_w = area.width.saturating_sub(6).min(80);
    if max_w < 12 {
        return;
    }

    let entries: Vec<(String, ratatui::style::Color)> = app
        .toasts
        .iter()
        .map(|toast| {
            let color = match toast.kind {
                ToastKind::Info => t.accent,
                ToastKind::Success => t.good,
                ToastKind::Warn => t.warn,
                ToastKind::Error => t.bad,
            };
            (truncate(&toast.text, max_w as usize - 4), color)
        })
        .collect();

    let height = entries.len() as u16 + 2;
    let width = entries.iter().map(|(s, _)| s.chars().count() as u16).max().unwrap_or(10) + 4;
    let width = width.min(max_w);

    if area.height < height + 1 {
        return;
    }
    let rect = Rect {
        x: area.x + area.width.saturating_sub(width + 1),
        y: area.y + area.height.saturating_sub(height + 1),
        width,
        height,
    };

    f.render_widget(Clear, rect);
    let lines: Vec<Line> = entries
        .into_iter()
        .map(|(text, color)| {
            Line::from(vec![
                Span::styled("● ", Style::default().fg(color)),
                Span::styled(text, t.text()),
            ])
        })
        .collect();

    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(ratatui::widgets::BorderType::Rounded)
                .border_style(Style::default().fg(t.border))
                .style(Style::default().bg(t.gauge_bg)),
        ),
        rect,
    );
}
