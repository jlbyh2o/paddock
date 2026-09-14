//! Reusable pieces of chrome: the tab bar, the status line, toasts, modals, a text
//! input, a scrollable list state, and the small meters the Dashboard is built from.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use serde::Serialize;

use super::theme::{bar, Theme};

// ---------------------------------------------------------------- selection

/// Cursor plus scroll offset for a list of `len` items. Kept separate from ratatui's
/// `ListState` because most views render tables and need the offset arithmetic anyway.
#[derive(Debug, Clone, Default)]
pub struct Selection {
    pub index: usize,
    pub offset: usize,
}

impl Selection {
    pub fn clamp(&mut self, len: usize) {
        if len == 0 {
            self.index = 0;
            self.offset = 0;
        } else if self.index >= len {
            self.index = len - 1;
        }
    }

    pub fn up(&mut self, len: usize) {
        if len == 0 {
            return;
        }
        self.index = if self.index == 0 { len - 1 } else { self.index - 1 };
    }

    pub fn down(&mut self, len: usize) {
        if len == 0 {
            return;
        }
        self.index = (self.index + 1) % len;
    }

    pub fn page_up(&mut self, page: usize) {
        self.index = self.index.saturating_sub(page.max(1));
    }

    pub fn page_down(&mut self, len: usize, page: usize) {
        if len == 0 {
            return;
        }
        self.index = (self.index + page.max(1)).min(len - 1);
    }

    pub fn first(&mut self) {
        self.index = 0;
    }

    pub fn last(&mut self, len: usize) {
        self.index = len.saturating_sub(1);
    }

    /// Keep the cursor inside a viewport of `height` rows and return the row range to
    /// render.
    pub fn window(&mut self, len: usize, height: usize) -> std::ops::Range<usize> {
        if height == 0 || len == 0 {
            self.offset = 0;
            return 0..0;
        }
        if self.index < self.offset {
            self.offset = self.index;
        } else if self.index >= self.offset + height {
            self.offset = self.index + 1 - height;
        }
        let max_offset = len.saturating_sub(height);
        self.offset = self.offset.min(max_offset);
        self.offset..(self.offset + height).min(len)
    }
}

// ---------------------------------------------------------------- text input

/// A single-line editable field with a cursor. Enough for search boxes, paths and knob
/// values; deliberately not a full editor.
#[derive(Debug, Clone, Default)]
pub struct TextInput {
    pub value: String,
    pub cursor: usize,
}

impl TextInput {
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        let cursor = value.chars().count();
        Self { value, cursor }
    }

    pub fn set(&mut self, value: impl Into<String>) {
        self.value = value.into();
        self.cursor = self.value.chars().count();
    }

    pub fn clear(&mut self) {
        self.value.clear();
        self.cursor = 0;
    }

    fn byte_at(&self, char_idx: usize) -> usize {
        self.value.char_indices().nth(char_idx).map(|(b, _)| b).unwrap_or(self.value.len())
    }

    pub fn insert(&mut self, c: char) {
        let at = self.byte_at(self.cursor);
        self.value.insert(at, c);
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = self.byte_at(self.cursor - 1);
        let end = self.byte_at(self.cursor);
        self.value.replace_range(start..end, "");
        self.cursor -= 1;
    }

    pub fn delete(&mut self) {
        let len = self.value.chars().count();
        if self.cursor >= len {
            return;
        }
        let start = self.byte_at(self.cursor);
        let end = self.byte_at(self.cursor + 1);
        self.value.replace_range(start..end, "");
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.value.chars().count());
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.value.chars().count();
    }

    /// Delete the word before the cursor (Ctrl-W).
    pub fn delete_word(&mut self) {
        let chars: Vec<char> = self.value.chars().collect();
        let mut i = self.cursor;
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !chars[i - 1].is_whitespace() {
            i -= 1;
        }
        let start = self.byte_at(i);
        let end = self.byte_at(self.cursor);
        self.value.replace_range(start..end, "");
        self.cursor = i;
    }

    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }

    /// Render as a `Line`, drawing the cursor as a reverse-video cell so no terminal
    /// cursor management is needed.
    pub fn line<'a>(&self, theme: &Theme, focused: bool, placeholder: &'a str) -> Line<'a> {
        if self.value.is_empty() && !focused {
            return Line::from(Span::styled(placeholder.to_string(), theme.muted()));
        }
        if !focused {
            return Line::from(Span::styled(self.value.clone(), theme.text()));
        }
        let chars: Vec<char> = self.value.chars().collect();
        let before: String = chars[..self.cursor.min(chars.len())].iter().collect();
        let at: String =
            chars.get(self.cursor).map(|c| c.to_string()).unwrap_or_else(|| " ".into());
        let after: String =
            chars.get(self.cursor + 1..).map(|s| s.iter().collect()).unwrap_or_default();
        Line::from(vec![
            Span::styled(before, theme.text()),
            Span::styled(at, Style::default().add_modifier(Modifier::REVERSED)),
            Span::styled(after, theme.text()),
        ])
    }
}

// ---------------------------------------------------------------- toasts

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToastKind {
    Info,
    Success,
    Warn,
    Error,
}

#[derive(Debug, Clone)]
pub struct Toast {
    /// Monotonic for the life of the process, so a browser can animate one toast in and
    /// another out without matching on their text.
    pub id: u64,
    pub text: String,
    pub kind: ToastKind,
    pub at: std::time::Instant,
}

static NEXT_TOAST_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl Toast {
    pub fn new(text: impl Into<String>, kind: ToastKind) -> Self {
        Self {
            id: NEXT_TOAST_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            text: text.into(),
            kind,
            at: std::time::Instant::now(),
        }
    }

    /// How long this toast is meant to stay up. Sent to the browser so it can fade in
    /// step with the server expiring it rather than guessing.
    pub fn ttl(&self) -> std::time::Duration {
        match self.kind {
            ToastKind::Error => std::time::Duration::from_secs(12),
            ToastKind::Warn => std::time::Duration::from_secs(8),
            _ => std::time::Duration::from_secs(4),
        }
    }

    /// Errors linger; routine confirmations do not.
    pub fn is_expired(&self) -> bool {
        self.at.elapsed() > self.ttl()
    }
}

// ---------------------------------------------------------------- meters

/// A labelled horizontal meter: `LABEL ▕████····▏ 62%  detail`.
pub fn meter_line<'a>(
    theme: &Theme,
    label: &str,
    ratio: f64,
    width: usize,
    detail: impl Into<String>,
) -> Line<'a> {
    let color = theme.for_ratio(ratio);
    Line::from(vec![
        Span::styled(format!("{label:<10}"), theme.label()),
        Span::styled("▕", theme.muted()),
        Span::styled(bar(ratio, width), Style::default().fg(color)),
        Span::styled("▏", theme.muted()),
        Span::styled(format!(" {:>3.0}% ", ratio * 100.0), Style::default().fg(color)),
        Span::styled(detail.into(), theme.muted()),
    ])
}

/// Center a rectangle of `width` x `height` inside `area`, shrinking to fit.
pub fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width.saturating_sub(2)).max(1);
    let h = height.min(area.height.saturating_sub(2)).max(1);
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}

// ---------------------------------------------------------------- modal

/// Draw a modal dialog over `area` and return its inner content rect.
pub fn modal(
    f: &mut Frame,
    theme: &Theme,
    area: Rect,
    title: &str,
    width: u16,
    height: u16,
) -> Rect {
    let rect = centered(area, width, height);
    f.render_widget(Clear, rect);
    let block = theme.pane(title, true).style(Style::default().bg(theme.gauge_bg).fg(theme.fg));
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    inner
}

/// A yes/no (or multi-choice) confirmation.
#[derive(Debug, Clone)]
pub struct Confirm {
    pub title: String,
    pub body: Vec<String>,
    pub options: Vec<String>,
    pub selected: usize,
    /// Opaque tag the view uses to route the answer.
    pub action: ConfirmAction,
    /// Whether the affirmative option is destructive, which colors it.
    pub destructive: bool,
}

/// What a confirmation, once accepted, should do. Kept as a closed enum so the answer is
/// routed by the app rather than by a boxed callback that would tangle borrowing.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfirmAction {
    StopEngine {
        force: bool,
    },
    DeleteModel(std::path::PathBuf),
    CancelJob(u64),
    CancelDownload(u64),
    DeleteProfile(String),
    Quit,
    ApplyCacheRebuild,
    /// Write a stored template into a model's directories.
    ApplyTemplate {
        template: String,
        model: std::path::PathBuf,
    },
    /// Restore a model's own template.
    RevertTemplate(std::path::PathBuf),
    DeleteTemplate(String),
    /// Pull the FreeToken checkout and reinstall it into its venv.
    UpdateFreetoken,
    /// Delete a failed conversion's leftovers, then convert `source` again.
    ReconvertModel(std::path::PathBuf),
    /// Run Hugging Face's installer for the `hf` CLI.
    InstallHfCli,
    /// Convert `source` even though the preflight raised a concern.
    ConvertAnyway(std::path::PathBuf),
}

// Written out because most variants carry an unnamed payload that internal tagging
// cannot place; the field names are the ones docs/web-api.md fixes.
impl Serialize for ConfirmAction {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = s.serialize_map(None)?;
        match self {
            ConfirmAction::StopEngine { force } => {
                m.serialize_entry("kind", "stop_engine")?;
                m.serialize_entry("force", force)?;
            }
            ConfirmAction::DeleteModel(path) => {
                m.serialize_entry("kind", "delete_model")?;
                m.serialize_entry("path", path)?;
            }
            ConfirmAction::CancelJob(id) => {
                m.serialize_entry("kind", "cancel_job")?;
                m.serialize_entry("id", id)?;
            }
            ConfirmAction::CancelDownload(id) => {
                m.serialize_entry("kind", "cancel_download")?;
                m.serialize_entry("id", id)?;
            }
            ConfirmAction::DeleteProfile(name) => {
                m.serialize_entry("kind", "delete_profile")?;
                m.serialize_entry("name", name)?;
            }
            ConfirmAction::Quit => m.serialize_entry("kind", "quit")?,
            ConfirmAction::ApplyCacheRebuild => m.serialize_entry("kind", "apply_cache_rebuild")?,
            ConfirmAction::UpdateFreetoken => m.serialize_entry("kind", "update_freetoken")?,
            ConfirmAction::ApplyTemplate { template, model } => {
                m.serialize_entry("kind", "apply_template")?;
                m.serialize_entry("template", template)?;
                m.serialize_entry("model", model)?;
            }
            ConfirmAction::RevertTemplate(model) => {
                m.serialize_entry("kind", "revert_template")?;
                m.serialize_entry("model", model)?;
            }
            ConfirmAction::DeleteTemplate(name) => {
                m.serialize_entry("kind", "delete_template")?;
                m.serialize_entry("name", name)?;
            }
            ConfirmAction::ReconvertModel(source) => {
                m.serialize_entry("kind", "reconvert_model")?;
                m.serialize_entry("source", source)?;
            }
            ConfirmAction::InstallHfCli => m.serialize_entry("kind", "install_hf_cli")?,
            ConfirmAction::ConvertAnyway(source) => {
                m.serialize_entry("kind", "convert_anyway")?;
                m.serialize_entry("source", source)?;
            }
        }
        m.end()
    }
}

impl Confirm {
    pub fn new(
        title: impl Into<String>,
        body: Vec<String>,
        action: ConfirmAction,
        destructive: bool,
    ) -> Self {
        Self {
            title: title.into(),
            body,
            options: vec!["Cancel".into(), "Confirm".into()],
            // Default to the safe option, so a stray Enter never destroys anything.
            selected: 0,
            action,
            destructive,
        }
    }

    pub fn accepted(&self) -> bool {
        self.selected == 1
    }

    pub fn render(&self, f: &mut Frame, theme: &Theme, area: Rect) {
        let body_lines = self.body.len() as u16;
        let inner = modal(f, theme, area, &self.title, 74, body_lines + 6);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1), Constraint::Length(1)])
            .split(inner);

        let text: Vec<Line> =
            self.body.iter().map(|l| Line::from(Span::styled(l.clone(), theme.text()))).collect();
        f.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), chunks[0]);

        let mut spans = Vec::new();
        for (i, opt) in self.options.iter().enumerate() {
            let selected = i == self.selected;
            let danger = self.destructive && i == 1;
            let style = if selected {
                let base = Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED);
                if danger {
                    base.fg(theme.bad)
                } else {
                    base.fg(theme.accent)
                }
            } else {
                theme.muted()
            };
            spans.push(Span::styled(format!("  {opt}  "), style));
            spans.push(Span::raw(" "));
        }
        f.render_widget(Paragraph::new(Line::from(spans)).alignment(Alignment::Center), chunks[2]);
    }
}

// ---------------------------------------------------------------- footer

/// Render the key hints across the bottom of the screen.
pub fn footer(f: &mut Frame, theme: &Theme, area: Rect, hints: &[(&str, &str)]) {
    let mut spans = vec![Span::raw(" ")];
    for (key, what) in hints {
        spans.extend(theme.key_hint(key, what));
    }
    // Measured before rendering, so the version can be placed only where it will not land
    // on top of a hint. The hints are context-sensitive and the terminal may be 40 columns
    // wide; both change how much room is left.
    let hints_width: usize = spans.iter().map(|s| s.content.chars().count()).sum();

    f.render_widget(
        Paragraph::new(Line::from(spans)).block(
            Block::default().borders(Borders::TOP).border_style(Style::default().fg(theme.border)),
        ),
        area,
    );

    // Bottom-right, on the hint line rather than the border row above it.
    let version = concat!("v", env!("CARGO_PKG_VERSION"));
    let width = version.chars().count() as u16;
    if area.height < 2 || hints_width + version.chars().count() + 2 > area.width as usize {
        return;
    }
    let rect =
        Rect { x: area.x + area.width - width - 1, y: area.y + area.height - 1, width, height: 1 };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(version, theme.muted())))
            .alignment(Alignment::Right),
        rect,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_wraps_at_both_ends() {
        let mut s = Selection::default();
        s.up(3);
        assert_eq!(s.index, 2);
        s.down(3);
        assert_eq!(s.index, 0);
    }

    #[test]
    fn selection_window_scrolls_to_keep_the_cursor_visible() {
        let mut s = Selection { index: 12, ..Default::default() };
        let range = s.window(20, 5);
        assert!(range.contains(&12), "{range:?}");
        assert_eq!(range.len(), 5);

        s.index = 0;
        let range = s.window(20, 5);
        assert_eq!(range, 0..5);
    }

    #[test]
    fn selection_window_never_scrolls_past_the_end() {
        let mut s = Selection { index: 19, ..Default::default() };
        let range = s.window(20, 8);
        assert_eq!(range, 12..20);
    }

    #[test]
    fn text_input_edits_multibyte_text_correctly() {
        let mut t = TextInput::new("héllo");
        assert_eq!(t.cursor, 5);
        t.left();
        t.backspace();
        assert_eq!(t.value, "hélo");
        t.home();
        t.insert('X');
        assert_eq!(t.value, "Xhélo");
        assert_eq!(t.cursor, 1);
    }

    #[test]
    fn delete_word_removes_the_preceding_word() {
        let mut t = TextInput::new("Qwen/Qwen3.6 35B");
        t.delete_word();
        assert_eq!(t.value, "Qwen/Qwen3.6 ");
        t.delete_word();
        assert_eq!(t.value, "");
    }

    #[test]
    fn delete_at_the_end_is_a_no_op() {
        let mut t = TextInput::new("ab");
        t.delete();
        assert_eq!(t.value, "ab");
        t.home();
        t.delete();
        assert_eq!(t.value, "b");
    }

    #[test]
    fn confirmations_default_to_the_safe_option() {
        let c = Confirm::new("Delete", vec![], ConfirmAction::Quit, true);
        assert!(!c.accepted());
    }
}
