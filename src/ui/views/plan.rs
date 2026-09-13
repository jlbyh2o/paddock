//! The plan overlay: what ft-man would change about this serve configuration, and why.
//!
//! Every row is a knob and a reason, because a recommendation nobody can check is worth
//! less than no recommendation at all — the reasons quote the numbers they were derived
//! from so a wrong one is visibly wrong rather than quietly followed.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::plan::{Level, Plan};
use crate::ui::app::App;
use crate::ui::widgets::modal;

pub fn render(f: &mut Frame, app: &App, area: Rect) {
    let Some(plan) = app.serve_view.plan.as_ref() else { return };
    let t = &app.theme;
    let inner = modal(f, t, area, "Plan for this hardware", 92, 30);

    let mut lines: Vec<Line> = Vec::new();

    // The headline is the context, because that is the question the plan exists to answer.
    if let Some(fit) = plan.fit {
        let color = if fit.is_truncated() { t.warn } else { t.good };
        lines.push(Line::from(vec![
            Span::styled("Context after this plan  ", t.label()),
            Span::styled(fit.verdict(), Style::default().fg(color).add_modifier(Modifier::BOLD)),
        ]));
        lines.push(Line::from(""));
    }

    if let Some(why) = &plan.unpriced {
        lines.push(Line::from(Span::styled(
            format!("The cache split was not planned: {why}."),
            Style::default().fg(t.warn),
        )));
        lines.push(Line::from(""));
    }

    if plan.is_empty() {
        lines.push(Line::from(Span::styled(
            "Nothing to change — this configuration is already what the plan would pick.",
            Style::default().fg(t.good),
        )));
    }

    let width = inner.width as usize;
    for step in &plan.steps {
        let color = match step.level {
            Level::Info => t.dim,
            Level::Advice => t.accent,
            Level::Warning => t.warn,
        };
        let marker = match step.level {
            Level::Info => "·",
            Level::Advice => "→",
            Level::Warning => "!",
        };
        // A step that sets a knob leads with the flag; a step that only has something to
        // say leads with the sentence, because "— " as a heading is just noise.
        match &step.set {
            Some(_) => {
                lines.push(Line::from(vec![
                    Span::styled(format!("{marker} "), Style::default().fg(color)),
                    Span::styled(
                        step.label(),
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    ),
                ]));
                for row in wrap(&step.reason, width.saturating_sub(2)) {
                    lines.push(Line::from(vec![Span::raw("  "), Span::styled(row, t.muted())]));
                }
            }
            None => {
                for (i, row) in wrap(&step.reason, width.saturating_sub(2)).into_iter().enumerate()
                {
                    lines.push(Line::from(vec![
                        Span::styled(
                            if i == 0 { format!("{marker} ") } else { "  ".into() },
                            Style::default().fg(color),
                        ),
                        Span::styled(
                            row,
                            if i == 0 { Style::default().fg(color) } else { t.muted() },
                        ),
                    ]));
                }
            }
        }
        lines.push(Line::from(""));
    }

    let edits = plan.edits().len();
    lines.push(Line::from(Span::styled(
        if edits == 0 {
            "Esc to close.".to_string()
        } else {
            format!(
                "A applies {edits} change{} to the Serve configuration; Esc closes without \
                 touching it.",
                if edits == 1 { "" } else { "s" }
            )
        },
        t.text(),
    )));

    f.render_widget(Paragraph::new(lines), inner);
}

/// Break `text` into lines of at most `width` columns on word boundaries.
///
/// ratatui's own `Wrap` restarts every continuation at column zero, which un-indents the
/// body of each step and makes the list hard to scan. Wrapping here keeps the hanging
/// indent the caller applies.
fn wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut out: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let need = if line.is_empty() {
            word.chars().count()
        } else {
            line.chars().count() + 1 + word.chars().count()
        };
        if need > width && !line.is_empty() {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push(line);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// Build a plan for whatever the Serve tab is currently configured to launch.
///
/// Everything it needs is already in the app: the model from the configuration, the
/// hardware from NVML, the bench profile from disk, and the per-unit VRAM costs from the
/// running engine or the store of what an earlier serve measured.
pub fn build(app: &App) -> Result<Plan, String> {
    let model = app
        .serve
        .get("model")
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .ok_or("no model is configured — set one on the Serve or Models tab")?;

    // The name the engine will answer to, which is the key costs are remembered under.
    let served = app
        .serve
        .get("served_model_name")
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| basename(model));

    let library = app.models.iter().find(|m| m.path.to_string_lossy() == model);
    let target = crate::plan::Target {
        name: basename(model).to_string(),
        ceiling: app
            .serve
            .get("max_seq_len_override")
            .and_then(|v| v.parse::<u64>().ok())
            .or_else(|| library.and_then(|m| m.max_position)),
        is_moe: library.is_some_and(|m| m.num_experts.is_some_and(|n| n > 0)),
        quant: library.and_then(|m| m.quant.clone()),
    };

    let gpu = app.gpus.first();
    let machine = crate::plan::Machine {
        gpu_name: gpu.map(|g| g.name.clone()),
        host_ram_total: app.host.memory_total,
        host_ram_available: app.host.memory_free(),
        physical_cores: app.host.physical_cores,
        pcie_link: gpu.and_then(|g| g.pcie_link.clone()),
    };

    let costs = app.costs_for(served);
    Ok(crate::plan::build(
        &target,
        &machine,
        costs.as_ref(),
        app.bench_profile.as_ref(),
        &app.serve,
    ))
}

/// The last path component, which is what FreeToken names a serve after.
fn basename(model: &str) -> &str {
    model.trim_end_matches('/').rsplit('/').next().unwrap_or(model)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapping_breaks_on_words_and_never_exceeds_the_width() {
        let text = "reserve the full 233.1k of context before the expert cache takes the rest";
        for width in [12, 20, 40, 80] {
            let rows = wrap(text, width);
            assert!(rows.iter().all(|r| r.chars().count() <= width), "width {width} overflowed");
            assert_eq!(rows.join(" "), text, "no word may be lost or duplicated");
        }
    }

    #[test]
    fn a_word_longer_than_the_width_is_kept_rather_than_dropped() {
        let rows = wrap("supercalifragilistic x", 5);
        assert_eq!(rows, vec!["supercalifragilistic", "x"]);
    }

    #[test]
    fn wrapping_degenerate_input_still_yields_a_line() {
        assert_eq!(wrap("", 20), vec![String::new()]);
        assert_eq!(wrap("text", 0), vec!["text".to_string()]);
    }

    #[test]
    fn a_served_name_is_the_last_path_component() {
        assert_eq!(basename("/models/Qwen3.6-35B"), "Qwen3.6-35B");
        assert_eq!(basename("/models/Qwen3.6-35B/"), "Qwen3.6-35B");
        assert_eq!(basename("Qwen/Qwen3.6-35B"), "Qwen3.6-35B");
        assert_eq!(basename("bare"), "bare");
    }
}
