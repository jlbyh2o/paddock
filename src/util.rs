//! Small formatting and math helpers shared by every view.

/// Format a byte count with binary units, e.g. `12.4 GiB`.
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64;
    let mut i = 0usize;
    while v >= 1024.0 && i + 1 < UNITS.len() {
        v /= 1024.0;
        i += 1;
    }
    if v >= 100.0 {
        format!("{v:.0} {}", UNITS[i])
    } else if v >= 10.0 {
        format!("{v:.1} {}", UNITS[i])
    } else {
        format!("{v:.2} {}", UNITS[i])
    }
}

/// Format a byte count in a fixed-width-friendly short form, e.g. `12.4G`.
pub fn bytes_short(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "K", "M", "G", "T", "P"];
    if n < 1024 {
        return format!("{n}B");
    }
    let mut v = n as f64;
    let mut i = 0usize;
    while v >= 1024.0 && i + 1 < UNITS.len() {
        v /= 1024.0;
        i += 1;
    }
    if v >= 10.0 {
        format!("{v:.0}{}", UNITS[i])
    } else {
        format!("{v:.1}{}", UNITS[i])
    }
}

/// Format a count with thousands separators, e.g. `1,234,567`.
pub fn count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Compact duration, e.g. `3d 4h`, `12m 30s`, `45s`.
pub fn duration_secs(secs: u64) -> String {
    let d = secs / 86_400;
    let h = (secs % 86_400) / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

/// Human transfer rate from bytes per second.
pub fn rate(bytes_per_sec: f64) -> String {
    if !bytes_per_sec.is_finite() || bytes_per_sec <= 0.0 {
        return "--".into();
    }
    format!("{}/s", bytes(bytes_per_sec as u64))
}

/// Estimated time remaining from a remaining byte count and a rate.
pub fn eta(remaining: u64, bytes_per_sec: f64) -> String {
    if !bytes_per_sec.is_finite() || bytes_per_sec <= 1.0 {
        return "--".into();
    }
    duration_secs((remaining as f64 / bytes_per_sec) as u64)
}

/// Truncate to `max` display columns, appending an ellipsis when cut.
pub fn truncate(s: &str, max: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    if max == 0 {
        return String::new();
    }
    let mut width = 0usize;
    let mut total = 0usize;
    for c in s.chars() {
        total += c.width().unwrap_or(0);
    }
    if total <= max {
        return s.to_string();
    }
    let mut out = String::new();
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if width + w > max.saturating_sub(1) {
            break;
        }
        width += w;
        out.push(c);
    }
    out.push('…');
    out
}

/// Truncate from the left, keeping the tail (useful for long paths).
pub fn truncate_left(s: &str, max: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    if s.width() <= max || max == 0 {
        return s.to_string();
    }
    let keep = max.saturating_sub(1);
    let mut out: Vec<char> = Vec::new();
    let mut width = 0usize;
    for c in s.chars().rev() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if width + w > keep {
            break;
        }
        width += w;
        out.push(c);
    }
    out.push('…');
    out.into_iter().rev().collect()
}

/// Clamp a ratio into `0.0..=1.0`, mapping non-finite values to 0.
/// Break a line into pieces no wider than `max` columns, on spaces where there is one.
///
/// For prose the terminal has to show whole — a model's summary, where the tail of a
/// sentence is not the part worth dropping. Everything else here truncates instead,
/// because a field's value has a shape and a wrapped one loses it.
///
/// A word longer than `max` (a path, a flag) is emitted on its own line over-long rather
/// than cut: the reader can at least copy it.
pub fn wrap(line: &str, max: usize) -> Vec<String> {
    if max == 0 {
        return vec![line.to_string()];
    }
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in line.split_whitespace() {
        let width = current.chars().count();
        if current.is_empty() {
            current.push_str(word);
        } else if width + 1 + word.chars().count() <= max {
            current.push(' ');
            current.push_str(word);
        } else {
            out.push(std::mem::take(&mut current));
            current.push_str(word);
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    // A blank line is a paragraph break and has to survive as one.
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

pub fn ratio(num: u64, den: u64) -> f64 {
    if den == 0 {
        return 0.0;
    }
    (num as f64 / den as f64).clamp(0.0, 1.0)
}

/// A tiny exponential moving average used to smooth polled rates.
#[derive(Debug, Clone, Copy)]
pub struct Ema {
    value: Option<f64>,
    alpha: f64,
}

impl Ema {
    pub fn new(alpha: f64) -> Self {
        Self { value: None, alpha }
    }
    pub fn push(&mut self, sample: f64) -> f64 {
        let next = match self.value {
            Some(v) => v * (1.0 - self.alpha) + sample * self.alpha,
            None => sample,
        };
        self.value = Some(next);
        next
    }
    pub fn get(&self) -> f64 {
        self.value.unwrap_or(0.0)
    }
}

/// A fixed-capacity ring of samples backing the sparklines.
#[derive(Debug, Clone)]
pub struct History {
    buf: Vec<u64>,
    cap: usize,
}

impl History {
    pub fn new(cap: usize) -> Self {
        Self { buf: Vec::with_capacity(cap), cap }
    }
    pub fn push(&mut self, v: u64) {
        if self.buf.len() == self.cap {
            self.buf.remove(0);
        }
        self.buf.push(v);
    }
    /// The most recent `n` samples, oldest first.
    pub fn tail(&self, n: usize) -> &[u64] {
        let start = self.buf.len().saturating_sub(n);
        &self.buf[start..]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wrapping is for prose, so the words have to survive and the width has to hold.
    #[test]
    fn wrap_breaks_on_spaces_and_keeps_every_word() {
        let line = "the engine now serves image input on the Qwen families";
        let pieces = wrap(line, 20);
        assert!(pieces.iter().all(|p| p.chars().count() <= 20), "{pieces:?}");
        assert_eq!(pieces.join(" "), line, "no word is lost or duplicated");

        // A word wider than the column goes out whole rather than cut in half.
        let long = wrap("--allowed-local-media-path", 10);
        assert_eq!(long, vec!["--allowed-local-media-path"]);

        // A blank line is a paragraph break, not nothing.
        assert_eq!(wrap("", 20), vec![String::new()]);
    }
}
