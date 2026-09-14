//! The sampling-override editor, drawn over the Models tab.
//!
//! Three fields, because those are the three keys FreeToken's loader reads. The pane makes
//! a point of showing what the checkpoint recommends beside what is about to replace it:
//! the values are a property of the model, and an override that quietly diverges from the
//! one the authors shipped is exactly the thing worth seeing before it is written.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::ui::app::{App, SamplingField};
use crate::ui::widgets::modal;

pub fn render(f: &mut Frame, app: &App, area: Rect) {
    let Some(path) = app.sampling_view.model.as_ref() else { return };
    let t = &app.theme;
    let name = app
        .models
        .iter()
        .find(|m| &m.path == path)
        .map(|m| m.name.clone())
        .unwrap_or_else(|| path.display().to_string());

    let inner = modal(f, t, area, &format!("Sampling defaults — {name}"), 74, 20);
    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(Span::styled(
        "Written into the checkpoint's generation_config.json, which is where FreeToken",
        t.muted(),
    )));
    lines.push(Line::from(Span::styled(
        "reads the defaults it applies to any request that does not set its own.",
        t.muted(),
    )));
    lines.push(Line::from(""));

    for field in SamplingField::ALL {
        let focused = app.sampling_view.field == field;
        let mut spans = vec![
            Span::styled(
                if focused { "▌" } else { " " },
                Style::default().fg(if focused { t.accent } else { t.dim }),
            ),
            Span::styled(
                format!("{:<14}", field.label()),
                if focused { t.selected() } else { t.label() },
            ),
            Span::raw(" "),
        ];
        let input = match field {
            SamplingField::Temperature => &app.sampling_view.temperature,
            SamplingField::TopP => &app.sampling_view.top_p,
            SamplingField::TopK => &app.sampling_view.top_k,
        };
        spans.extend(input.line(t, focused, field.framework_default()).spans);
        lines.push(Line::from(spans));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "An empty field leaves the key out, so the engine falls back to the default shown.",
        t.muted(),
    )));

    // What is in force right now, so the override can be compared against it rather than
    // typed blind. Both come off the scan rather than the disk; `refresh_sampling_status`
    // puts them back in step after an apply or a revert.
    let model = app.models.iter().find(|m| &m.path == path);
    lines.push(Line::from(""));
    match model.and_then(|m| m.sampling_effective.as_ref()) {
        Some(current) => lines.push(t.field("Checkpoint now", current.summary())),
        None => lines.push(t.field("Checkpoint now", "recommends nothing")),
    }
    if let Some(crate::sampling::Status::Overridden(a)) = model.map(|m| &m.sampling_status) {
        lines.push(
            t.field("paddock override", format!("{} · {}", a.sampling.summary(), a.applied_at)),
        );
    }

    // Errors and warnings as the reader types, rather than on submit.
    lines.push(Line::from(""));
    match app.sampling_view.parse() {
        Err(problem) => lines.push(Line::from(Span::styled(
            problem,
            Style::default().fg(t.bad).add_modifier(Modifier::BOLD),
        ))),
        Ok(want) => {
            if let Some(problem) = want.validate() {
                lines.push(Line::from(Span::styled(
                    problem,
                    Style::default().fg(t.bad).add_modifier(Modifier::BOLD),
                )));
            } else {
                lines.push(t.field("Will serve", want.summary()));
                for warning in want.warnings() {
                    lines.push(Line::from(Span::styled(warning, Style::default().fg(t.warn))));
                }
            }
        }
    }

    f.render_widget(Paragraph::new(lines), inner);
}
