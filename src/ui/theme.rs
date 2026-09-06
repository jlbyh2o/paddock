//! The color palette and the shared chrome helpers every view draws with.
//!
//! Terminals vary wildly, so the palette sticks to a restrained set: one accent, one
//! muted tone for secondary text, and semantic colors for good/warn/bad. A `mono` theme
//! drops color entirely for terminals or recordings where it would be noise.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    Dark,
    Light,
    Mono,
}

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub variant: Variant,
    /// Primary body text.
    pub fg: Color,
    /// Secondary text: units, hints, inactive rows.
    pub dim: Color,
    /// Borders and rules.
    pub border: Color,
    /// Borders of the focused pane.
    pub border_focus: Color,
    pub accent: Color,
    pub good: Color,
    pub warn: Color,
    pub bad: Color,
    /// Background for the selected row.
    pub selection: Color,
    pub selection_fg: Color,
    /// Filled portion of gauges.
    pub gauge: Color,
    pub gauge_bg: Color,
}

impl Theme {
    pub fn from_name(name: &str) -> Self {
        match name {
            "light" => Self::light(),
            "mono" => Self::mono(),
            "dark" => Self::dark(),
            // `auto`: a light terminal is the rarer case and there is no portable way to
            // detect it, so honor the COLORFGBG hint some terminals set and default dark.
            _ => match std::env::var("COLORFGBG").ok().as_deref() {
                Some(v) if v.rsplit(';').next().is_some_and(|bg| matches!(bg, "7" | "15")) => {
                    Self::light()
                }
                _ => Self::dark(),
            },
        }
    }

    pub const fn dark() -> Self {
        Self {
            variant: Variant::Dark,
            fg: Color::Rgb(0xd8, 0xdc, 0xe4),
            dim: Color::Rgb(0x7c, 0x85, 0x96),
            border: Color::Rgb(0x3a, 0x41, 0x50),
            border_focus: Color::Rgb(0x6a, 0x9f, 0xd8),
            accent: Color::Rgb(0x6a, 0x9f, 0xd8),
            good: Color::Rgb(0x64, 0xb5, 0x7f),
            warn: Color::Rgb(0xd4, 0xa5, 0x54),
            bad: Color::Rgb(0xd4, 0x6a, 0x6a),
            selection: Color::Rgb(0x28, 0x35, 0x48),
            selection_fg: Color::Rgb(0xe8, 0xec, 0xf2),
            gauge: Color::Rgb(0x5a, 0x8f, 0xc8),
            gauge_bg: Color::Rgb(0x2a, 0x30, 0x3c),
        }
    }

    pub const fn light() -> Self {
        Self {
            variant: Variant::Light,
            fg: Color::Rgb(0x1c, 0x21, 0x2b),
            dim: Color::Rgb(0x60, 0x6a, 0x7a),
            border: Color::Rgb(0xc0, 0xc7, 0xd2),
            border_focus: Color::Rgb(0x1f, 0x66, 0xb0),
            accent: Color::Rgb(0x1f, 0x66, 0xb0),
            good: Color::Rgb(0x1e, 0x7a, 0x45),
            warn: Color::Rgb(0x9a, 0x6a, 0x10),
            bad: Color::Rgb(0xb0, 0x2b, 0x2b),
            selection: Color::Rgb(0xdc, 0xe6, 0xf4),
            selection_fg: Color::Rgb(0x10, 0x18, 0x24),
            gauge: Color::Rgb(0x2f, 0x76, 0xc0),
            gauge_bg: Color::Rgb(0xe2, 0xe6, 0xec),
        }
    }

    pub const fn mono() -> Self {
        Self {
            variant: Variant::Mono,
            fg: Color::Reset,
            dim: Color::DarkGray,
            border: Color::DarkGray,
            border_focus: Color::White,
            accent: Color::White,
            good: Color::Reset,
            warn: Color::Reset,
            bad: Color::White,
            selection: Color::DarkGray,
            selection_fg: Color::White,
            gauge: Color::White,
            gauge_bg: Color::DarkGray,
        }
    }

    // ---- common styles -------------------------------------------------

    pub fn text(&self) -> Style {
        Style::default().fg(self.fg)
    }
    pub fn muted(&self) -> Style {
        Style::default().fg(self.dim)
    }
    pub fn title(&self) -> Style {
        Style::default().fg(self.accent).add_modifier(Modifier::BOLD)
    }
    pub fn label(&self) -> Style {
        Style::default().fg(self.dim)
    }
    pub fn value(&self) -> Style {
        Style::default().fg(self.fg).add_modifier(Modifier::BOLD)
    }
    pub fn selected(&self) -> Style {
        Style::default().fg(self.selection_fg).bg(self.selection).add_modifier(Modifier::BOLD)
    }
    pub fn header(&self) -> Style {
        Style::default().fg(self.dim).add_modifier(Modifier::BOLD)
    }

    /// Color a 0..1 utilization figure: calm until it is worth noticing.
    pub fn for_ratio(&self, r: f64) -> Color {
        if r >= 0.92 {
            self.bad
        } else if r >= 0.75 {
            self.warn
        } else {
            self.good
        }
    }

    /// A bordered pane. `focused` brightens the border rather than changing the title,
    /// so focus is visible without the layout shifting.
    pub fn pane<'a>(&self, title: impl Into<String>, focused: bool) -> Block<'a> {
        let title: String = title.into();
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(if focused {
                self.border_focus
            } else {
                self.border
            }))
            .title(Line::from(vec![
                Span::styled(" ", self.muted()),
                Span::styled(title, if focused { self.title() } else { self.header() }),
                Span::styled(" ", self.muted()),
            ]))
    }

    /// `label  value` pair, the workhorse of every detail pane.
    pub fn field<'a>(&self, label: &'a str, value: impl Into<String>) -> Line<'a> {
        Line::from(vec![
            Span::styled(format!("{label:<18}"), self.label()),
            Span::styled(value.into(), self.value()),
        ])
    }

    /// `label  value` where the value carries its own color.
    pub fn field_colored<'a>(
        &self,
        label: &'a str,
        value: impl Into<String>,
        color: Color,
    ) -> Line<'a> {
        Line::from(vec![
            Span::styled(format!("{label:<18}"), self.label()),
            Span::styled(value.into(), Style::default().fg(color).add_modifier(Modifier::BOLD)),
        ])
    }

    /// A status marker plus its label. Colored everywhere except the `mono` theme,
    /// which distinguishes states by glyph instead so it stays readable without color.
    pub fn status_dot<'a>(&self, color: Color, text: impl Into<String>) -> Vec<Span<'a>> {
        let glyph = if self.variant == Variant::Mono {
            if color == self.bad {
                "! "
            } else if color == self.warn {
                "~ "
            } else {
                "· "
            }
        } else {
            "● "
        };
        vec![
            Span::styled(glyph, Style::default().fg(color)),
            Span::styled(text.into(), Style::default().fg(self.fg)),
        ]
    }

    /// An inline `key  description` hint for the footer.
    pub fn key_hint<'a>(&self, key: &'a str, what: &'a str) -> Vec<Span<'a>> {
        vec![
            Span::styled(key, Style::default().fg(self.accent).add_modifier(Modifier::BOLD)),
            Span::styled(" ", self.muted()),
            Span::styled(what, self.muted()),
            Span::styled("   ", self.muted()),
        ]
    }
}

/// Draw a horizontal bar into a string of `width` cells. Used where a full `Gauge`
/// widget would be too heavy — inside table rows, mostly.
pub fn bar(ratio: f64, width: usize) -> String {
    const BLOCKS: [char; 9] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];
    if width == 0 {
        return String::new();
    }
    let filled = (ratio.clamp(0.0, 1.0) * width as f64 * 8.0).round() as usize;
    let full = filled / 8;
    let rem = filled % 8;
    let mut s = String::with_capacity(width);
    for _ in 0..full.min(width) {
        s.push('█');
    }
    if full < width && rem > 0 {
        s.push(BLOCKS[rem]);
    }
    while s.chars().count() < width {
        s.push('·');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bars_are_exactly_the_requested_width() {
        for r in [0.0, 0.01, 0.5, 0.999, 1.0] {
            assert_eq!(bar(r, 10).chars().count(), 10, "ratio {r}");
        }
    }

    #[test]
    fn a_full_bar_is_all_blocks_and_an_empty_one_has_none() {
        assert_eq!(bar(1.0, 4), "████");
        assert!(!bar(0.0, 4).contains('█'));
    }

    #[test]
    fn ratio_colors_escalate() {
        let t = Theme::dark();
        assert_eq!(t.for_ratio(0.10), t.good);
        assert_eq!(t.for_ratio(0.80), t.warn);
        assert_eq!(t.for_ratio(0.99), t.bad);
    }
}
