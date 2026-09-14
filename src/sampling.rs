//! Sampling-default overrides.
//!
//! FreeToken has no flag for the sampling values themselves. `--sampling-defaults=model`
//! — its own default — makes the server read `generation_config.json` out of the model
//! directory once at boot (`load_generation_sampling`), and `none` turns the idea off
//! entirely; there is no `--default-temperature`. So the only way to choose the numbers is
//! to change what that read finds, which is the same shape of problem
//! [`crate::templates`] solves for the prompt template, and this module is deliberately
//! its twin: write into the checkpoint directory, but make it reversible and obvious.
//!
//! * whatever `generation_config.json` was there first is moved aside, never overwritten;
//! * the override is *merged over* that original rather than replacing it — the file also
//!   carries `eos_token_id` and friends, and a replacement that dropped the stop ids would
//!   leave the model generating past the end of its turn;
//! * a marker file records what was applied and whether there was an original, so
//!   [`status`] can report the truth even after ft-man restarts;
//! * [`revert`] puts it back exactly.
//!
//! Only `temperature`, `top_k` and `top_p` are offered, because those are the only three
//! keys anything downstream reads: `load_generation_sampling` looks for exactly them, and
//! FreeToken's `SamplingParams` carries exactly them. `min_p` and `repetition_penalty` are
//! not engine concepts here, and writing them would put a number in the file that nothing
//! would ever apply.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// The file `GenerationConfig.from_pretrained(model_path)` reads.
pub const CONFIG_FILE: &str = "generation_config.json";
/// Where the checkpoint's own generation config is parked while an override is in place.
pub const BACKUP_FILE: &str = "generation_config.json.ft-man-original";
/// Records what ft-man applied, so an override survives a restart visibly.
pub const MARKER_FILE: &str = ".ft-man-sampling.json";

/// The sampling defaults a request that specifies nothing will resolve to.
///
/// Every field is optional and `None` means *remove the key*, which lets the engine fall
/// back to its framework default (temperature 0.0, top_k -1, top_p 1.0) for that one
/// value rather than to whatever the checkpoint happened to recommend.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sampling {
    pub temperature: Option<f64>,
    pub top_k: Option<i64>,
    pub top_p: Option<f64>,
}

impl Sampling {
    pub fn is_empty(&self) -> bool {
        self.temperature.is_none() && self.top_k.is_none() && self.top_p.is_none()
    }

    /// True when these values mean "always take the most likely token".
    ///
    /// Matches FreeToken's own `SamplingParams::is_greedy` on the temperature arm, which
    /// is the one that decides how the override has to be spelled: a greedy override is
    /// written as `do_sample: false`, the form `load_generation_sampling` short-circuits
    /// on, rather than as a temperature the sampler would then have to divide by.
    pub fn is_greedy(&self) -> bool {
        matches!(self.temperature, Some(t) if t <= 0.0)
    }

    /// Read the three keys out of a `generation_config.json` body.
    ///
    /// `do_sample: false` reads back as greedy — temperature zero, nothing else — because
    /// that is what FreeToken will make of it.
    pub fn from_config(obj: &serde_json::Map<String, serde_json::Value>) -> Self {
        if obj.get("do_sample").and_then(serde_json::Value::as_bool) == Some(false) {
            return Self { temperature: Some(0.0), ..Default::default() };
        }
        Self {
            temperature: obj.get("temperature").and_then(serde_json::Value::as_f64),
            top_k: obj.get("top_k").and_then(serde_json::Value::as_i64),
            top_p: obj.get("top_p").and_then(serde_json::Value::as_f64),
        }
    }

    /// Reject values the engine would refuse or silently mangle. `None` when it is fine.
    pub fn validate(&self) -> Option<String> {
        if self.is_empty() {
            return Some("nothing to apply: set at least one of temperature, top_k, top_p".into());
        }
        if let Some(t) = self.temperature {
            if !(0.0..=2.0).contains(&t) {
                return Some(format!("temperature {t} is outside 0.0–2.0"));
            }
        }
        if let Some(p) = self.top_p {
            if !(0.0..=1.0).contains(&p) || p == 0.0 {
                return Some(format!("top_p {p} is outside 0.0–1.0 (and cannot be 0)"));
            }
        }
        if let Some(k) = self.top_k {
            if k != -1 && k < 1 {
                return Some(format!("top_k {k} must be -1 (off) or 1 or more"));
            }
        }
        None
    }

    /// Things worth saying out loud that are not errors.
    ///
    /// The one that actually bites: FreeToken resolves an absent temperature to the
    /// framework default of 0.0, which is greedy — and greedy decoding ignores top_k and
    /// top_p entirely. So a top_p set without a temperature does nothing at all, which is
    /// not obvious from the file.
    pub fn warnings(&self) -> Vec<String> {
        let mut out = Vec::new();
        let filters = self.top_k.is_some() || self.top_p.is_some();
        if filters && self.temperature.is_none() {
            out.push(
                "temperature is unset, so it resolves to 0.0 (greedy) and top_k/top_p will \
                 have no effect"
                    .into(),
            );
        }
        if filters && self.is_greedy() {
            out.push("temperature is 0 (greedy), so top_k/top_p will have no effect".into());
        }
        out
    }

    /// One line for a status field: `temperature 0.6  top_p 0.95`.
    ///
    /// Deliberately the same shape as [`crate::ui::views::dashboard::format_sampling`],
    /// which prints what the *engine* reports, so the two can be compared by eye.
    pub fn summary(&self) -> String {
        if self.is_greedy() {
            return "greedy (temperature 0)".into();
        }
        let mut parts: Vec<String> = Vec::new();
        if let Some(t) = self.temperature {
            parts.push(format!("temperature {t}"));
        }
        if let Some(p) = self.top_p {
            parts.push(format!("top_p {p}"));
        }
        if let Some(k) = self.top_k {
            parts.push(format!("top_k {k}"));
        }
        if parts.is_empty() {
            "unset".into()
        } else {
            parts.join("  ")
        }
    }
}

// ---------------------------------------------------------------- per model

/// What ft-man recorded when it applied a sampling override to a checkpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppliedSampling {
    pub sampling: Sampling,
    pub applied_at: String,
    /// Whether the checkpoint had its own `generation_config.json` before this.
    pub had_original: bool,
}

#[derive(Debug, Clone, Default)]
pub enum Status {
    /// Whatever the checkpoint ships, untouched.
    #[default]
    Checkpoint,
    /// ft-man applied an override.
    Overridden(Box<AppliedSampling>),
}

// `{kind, label}` plus the applied record for an override, matching how
// `templates::Status` travels so both front ends can render them the same way.
impl Serialize for Status {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = s.serialize_map(None)?;
        match self {
            Status::Checkpoint => m.serialize_entry("kind", "checkpoint")?,
            Status::Overridden(applied) => {
                m.serialize_entry("kind", "overridden")?;
                m.serialize_entry("applied", applied)?;
            }
        }
        m.serialize_entry("label", &self.label())?;
        m.end()
    }
}

impl Status {
    pub fn is_overridden(&self) -> bool {
        matches!(self, Status::Overridden(_))
    }

    pub fn label(&self) -> String {
        match self {
            Status::Checkpoint => "checkpoint's own".into(),
            Status::Overridden(a) => a.sampling.summary(),
        }
    }
}

/// Whether a format can take an override at all.
///
/// GGUF is the one that cannot, and it fails *silently* rather than loudly, which is why
/// it is worth refusing up front: `load_generation_sampling` checks `gguf_config_source`
/// first and returns the `general.sampling.*` metadata out of the GGUF itself, so a
/// `generation_config.json` written beside it is never read.
pub fn unsupported(format: crate::models::Format) -> Option<&'static str> {
    match format {
        crate::models::Format::Gguf => Some(
            "GGUF carries its sampling in the file's own metadata (general.sampling.*), which \
             FreeToken reads before it looks at generation_config.json — an override here \
             would be ignored",
        ),
        crate::models::Format::PartialFtw => Some("this conversion is incomplete and cannot serve"),
        crate::models::Format::Hf | crate::models::Format::Ftw => None,
    }
}

/// What sampling a checkpoint directory will actually hand the engine.
///
/// Reads the file rather than the marker, so it is equally right for a checkpoint ft-man
/// has never touched — which is the whole point of showing it next to an override.
pub fn effective(model_dir: &Path) -> Option<Sampling> {
    let obj = read_object(&model_dir.join(CONFIG_FILE)).ok()?;
    let s = Sampling::from_config(&obj);
    (!s.is_empty()).then_some(s)
}

/// Whether an override is in place, and what it was.
pub fn status(model_dir: &Path) -> Status {
    let marker = model_dir.join(MARKER_FILE);
    if let Ok(raw) = std::fs::read_to_string(&marker) {
        if let Ok(applied) = serde_json::from_str::<AppliedSampling>(&raw) {
            // Trust the marker only while the file it describes is still there; someone
            // may have deleted it by hand, or a `hf download` may have replaced it.
            if model_dir.join(CONFIG_FILE).is_file() {
                return Status::Overridden(Box::new(applied));
            }
        }
    }
    Status::Checkpoint
}

/// Apply sampling defaults to a checkpoint directory.
///
/// Re-applying over an existing ft-man override does not re-back-up: the first backup is
/// the checkpoint's genuine original, and clobbering it with a previous override would
/// make [`revert`] restore the wrong thing. The new file is always built by merging over
/// that original, never over the override it replaces, so applying twice lands in the same
/// place as applying once.
pub fn apply(model_dir: &Path, want: &Sampling) -> Result<()> {
    anyhow::ensure!(model_dir.is_dir(), "{} is not a directory", model_dir.display());
    if let Some(problem) = want.validate() {
        anyhow::bail!("{problem}");
    }

    let target = model_dir.join(CONFIG_FILE);
    let backup = model_dir.join(BACKUP_FILE);
    let previous = status(model_dir);

    let had_original = match &previous {
        Status::Overridden(a) => a.had_original,
        Status::Checkpoint => {
            if target.is_file() {
                std::fs::rename(&target, &backup).with_context(|| {
                    format!("backing up the checkpoint's own config to {}", backup.display())
                })?;
                true
            } else {
                false
            }
        }
    };

    // Everything the checkpoint said that is not about sampling has to survive.
    let mut obj = if had_original {
        read_object(&backup).with_context(|| format!("reading {}", backup.display()))?
    } else {
        serde_json::Map::new()
    };

    if want.is_greedy() {
        // The form load_generation_sampling short-circuits on. Leaving a stale top_k/top_p
        // behind would be harmless there but misleading to anything else reading the file.
        obj.insert("do_sample".into(), false.into());
        obj.remove("temperature");
        obj.remove("top_k");
        obj.remove("top_p");
    } else {
        // Set it even when the checkpoint said nothing: a checkpoint that ships
        // `do_sample: false` makes the loader return greedy and ignore every value below.
        obj.insert("do_sample".into(), true.into());
        put(&mut obj, "temperature", want.temperature.map(num));
        put(&mut obj, "top_p", want.top_p.map(num));
        put(&mut obj, "top_k", want.top_k.map(serde_json::Value::from));
    }

    crate::config::write_atomic(&target, &serde_json::to_string_pretty(&obj)?)
        .with_context(|| format!("writing {}", target.display()))?;

    let applied = AppliedSampling {
        sampling: want.clone(),
        applied_at: chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        had_original,
    };
    crate::config::write_atomic(
        &model_dir.join(MARKER_FILE),
        &serde_json::to_string_pretty(&applied)?,
    )?;
    Ok(())
}

/// Undo an override, restoring the checkpoint exactly as it was.
pub fn revert(model_dir: &Path) -> Result<()> {
    let Status::Overridden(applied) = status(model_dir) else {
        anyhow::bail!("no ft-man sampling override is in place here");
    };
    let target = model_dir.join(CONFIG_FILE);
    let backup = model_dir.join(BACKUP_FILE);

    if applied.had_original {
        anyhow::ensure!(
            backup.is_file(),
            "the checkpoint's original generation config is missing from {}",
            backup.display()
        );
        std::fs::rename(&backup, &target)
            .with_context(|| format!("restoring {}", target.display()))?;
    } else if target.is_file() {
        // The checkpoint never had one; a leftover file would keep feeding the engine
        // ft-man's numbers, so it has to go.
        std::fs::remove_file(&target).with_context(|| format!("removing {}", target.display()))?;
    }
    let _ = std::fs::remove_file(model_dir.join(MARKER_FILE));
    Ok(())
}

// ---------------------------------------------------------------- helpers

fn read_object(path: &Path) -> Result<serde_json::Map<String, serde_json::Value>> {
    let raw = std::fs::read_to_string(path)?;
    let value: serde_json::Value = serde_json::from_str(&raw)
        .with_context(|| format!("{} is not valid JSON", path.display()))?;
    match value {
        serde_json::Value::Object(map) => Ok(map),
        _ => anyhow::bail!("{} is not a JSON object", path.display()),
    }
}

/// Insert a value, or remove the key when there is none — `None` means "let the engine use
/// its own default", which a null in the file would not say.
fn put(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    value: Option<serde_json::Value>,
) {
    match value {
        Some(v) => {
            obj.insert(key.into(), v);
        }
        None => {
            obj.remove(key);
        }
    }
}

/// A float that survives the round trip through `serde_json`.
fn num(v: f64) -> serde_json::Value {
    serde_json::Number::from_f64(v)
        .map(serde_json::Value::Number)
        .unwrap_or(serde_json::Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ft-man-sampling-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).unwrap();
    }

    fn read(dir: &Path, name: &str) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(dir.join(name)).unwrap()).unwrap()
    }

    fn want() -> Sampling {
        Sampling { temperature: Some(0.6), top_k: Some(20), top_p: Some(0.95) }
    }

    #[test]
    fn apply_preserves_everything_that_is_not_sampling() {
        let dir = tmpdir("preserve");
        write(&dir, CONFIG_FILE, r#"{"eos_token_id": [1, 2], "temperature": 1.0, "top_p": 0.8}"#);

        apply(&dir, &want()).unwrap();

        let out = read(&dir, CONFIG_FILE);
        // The stop ids are the reason this is a merge and not a replacement.
        assert_eq!(out["eos_token_id"], serde_json::json!([1, 2]));
        assert_eq!(out["temperature"], serde_json::json!(0.6));
        assert_eq!(out["top_p"], serde_json::json!(0.95));
        assert_eq!(out["top_k"], serde_json::json!(20));
        assert_eq!(out["do_sample"], serde_json::json!(true));
    }

    #[test]
    fn the_original_is_moved_aside_not_overwritten() {
        let dir = tmpdir("backup");
        write(&dir, CONFIG_FILE, r#"{"temperature": 1.0}"#);

        apply(&dir, &want()).unwrap();

        assert_eq!(read(&dir, BACKUP_FILE), serde_json::json!({"temperature": 1.0}));
        assert!(status(&dir).is_overridden());
    }

    #[test]
    fn reapplying_merges_over_the_original_not_over_the_last_override() {
        let dir = tmpdir("reapply");
        write(&dir, CONFIG_FILE, r#"{"eos_token_id": 7, "temperature": 1.0}"#);

        apply(&dir, &want()).unwrap();
        apply(&dir, &Sampling { temperature: Some(0.3), ..Default::default() }).unwrap();

        let out = read(&dir, CONFIG_FILE);
        assert_eq!(out["temperature"], serde_json::json!(0.3));
        assert_eq!(out["eos_token_id"], serde_json::json!(7));
        // Dropped from the second override, so it must not linger from the first.
        assert!(out.get("top_k").is_none(), "stale top_k survived a re-apply: {out}");
        assert!(out.get("top_p").is_none(), "stale top_p survived a re-apply: {out}");
        // And the backup is still the checkpoint's, not the first override's.
        assert_eq!(read(&dir, BACKUP_FILE)["temperature"], serde_json::json!(1.0));
    }

    #[test]
    fn revert_restores_the_original_exactly() {
        let dir = tmpdir("revert");
        let original = r#"{"eos_token_id": 7, "temperature": 1.0}"#;
        write(&dir, CONFIG_FILE, original);

        apply(&dir, &want()).unwrap();
        revert(&dir).unwrap();

        assert_eq!(std::fs::read_to_string(dir.join(CONFIG_FILE)).unwrap(), original);
        assert!(!dir.join(BACKUP_FILE).exists());
        assert!(!dir.join(MARKER_FILE).exists());
        assert!(!status(&dir).is_overridden());
    }

    #[test]
    fn revert_removes_the_file_when_the_checkpoint_never_had_one() {
        let dir = tmpdir("revert-none");

        apply(&dir, &want()).unwrap();
        assert!(dir.join(CONFIG_FILE).is_file());

        revert(&dir).unwrap();
        // Leaving an empty or stale file behind would keep feeding the engine our numbers.
        assert!(!dir.join(CONFIG_FILE).exists());
    }

    #[test]
    fn a_greedy_override_is_written_as_do_sample_false() {
        let dir = tmpdir("greedy");
        write(&dir, CONFIG_FILE, r#"{"eos_token_id": 7, "top_p": 0.8}"#);

        apply(&dir, &Sampling { temperature: Some(0.0), ..Default::default() }).unwrap();

        let out = read(&dir, CONFIG_FILE);
        assert_eq!(out["do_sample"], serde_json::json!(false));
        assert_eq!(out["eos_token_id"], serde_json::json!(7));
        // The loader ignores these once do_sample is false; leaving them would mislead.
        assert!(out.get("top_p").is_none(), "{out}");
    }

    #[test]
    fn a_sampling_override_overrules_a_greedy_checkpoint() {
        let dir = tmpdir("do-sample");
        // Without do_sample: true on the way out, load_generation_sampling short-circuits
        // on the checkpoint's false and never reads a single value ft-man wrote.
        write(&dir, CONFIG_FILE, r#"{"do_sample": false}"#);

        apply(&dir, &want()).unwrap();

        assert_eq!(read(&dir, CONFIG_FILE)["do_sample"], serde_json::json!(true));
    }

    #[test]
    fn status_ignores_a_marker_whose_file_has_gone() {
        let dir = tmpdir("stale-marker");
        write(&dir, CONFIG_FILE, "{}");
        apply(&dir, &want()).unwrap();

        std::fs::remove_file(dir.join(CONFIG_FILE)).unwrap();

        assert!(!status(&dir).is_overridden(), "a marker outlived the file it describes");
    }

    #[test]
    fn effective_reads_a_checkpoint_ft_man_never_touched() {
        let dir = tmpdir("effective");
        write(&dir, CONFIG_FILE, r#"{"temperature": 0.7, "top_k": 20}"#);

        let s = effective(&dir).unwrap();
        assert_eq!(s.temperature, Some(0.7));
        assert_eq!(s.top_k, Some(20));
        assert_eq!(s.top_p, None);
    }

    #[test]
    fn effective_reads_do_sample_false_the_way_the_engine_will() {
        let dir = tmpdir("effective-greedy");
        write(&dir, CONFIG_FILE, r#"{"do_sample": false, "temperature": 0.7}"#);

        assert!(effective(&dir).unwrap().is_greedy());
    }

    #[test]
    fn validate_rejects_what_the_engine_would_not_take() {
        assert!(Sampling::default().validate().is_some());
        assert!(Sampling { temperature: Some(3.0), ..Default::default() }.validate().is_some());
        assert!(Sampling { top_p: Some(1.5), ..Default::default() }.validate().is_some());
        assert!(Sampling { top_p: Some(0.0), ..Default::default() }.validate().is_some());
        assert!(Sampling { top_k: Some(0), ..Default::default() }.validate().is_some());
        assert!(Sampling { top_k: Some(-1), ..Default::default() }.validate().is_none());
        assert!(want().validate().is_none());
    }

    #[test]
    fn warnings_call_out_filters_that_cannot_fire() {
        assert!(!Sampling { top_p: Some(0.9), ..Default::default() }.warnings().is_empty());
        assert!(!Sampling { temperature: Some(0.0), top_k: Some(20), ..Default::default() }
            .warnings()
            .is_empty());
        assert!(want().warnings().is_empty());
    }

    #[test]
    fn gguf_is_refused_because_the_override_would_be_ignored() {
        assert!(unsupported(crate::models::Format::Gguf).is_some());
        assert!(unsupported(crate::models::Format::Hf).is_none());
        assert!(unsupported(crate::models::Format::Ftw).is_none());
    }
}

#[cfg(test)]
mod real_checkpoint {
    use super::*;

    /// A real Qwen-family `generation_config.json`, verbatim.
    ///
    /// The synthetic fixtures above are two or three keys wide; this is the shape that is
    /// actually on disk, and the one an override has to leave intact. `pad_token_id` and
    /// the two-entry `eos_token_id` are the fields that matter — FreeToken unions that list
    /// into its stop ids, and a model that loses them does not stop talking.
    const QWEN: &str = r#"{
  "bos_token_id": 151643,
  "do_sample": true,
  "eos_token_id": [
    151645,
    151643
  ],
  "max_length": 40960,
  "pad_token_id": 151654,
  "temperature": 0.6,
  "top_k": 20,
  "top_p": 0.95,
  "transformers_version": "5.5.4"
}"#;

    #[test]
    fn an_override_on_a_real_config_changes_only_the_sampling() {
        let dir = std::env::temp_dir().join(format!("ft-man-sampling-real-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(CONFIG_FILE), QWEN).unwrap();

        apply(&dir, &Sampling { temperature: Some(1.0), top_k: None, top_p: Some(0.8) }).unwrap();

        let out: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join(CONFIG_FILE)).unwrap()).unwrap();
        assert_eq!(out["temperature"], serde_json::json!(1.0));
        assert_eq!(out["top_p"], serde_json::json!(0.8));
        // Dropped from the override, so dropped from the file: the engine falls back to
        // top_k -1 rather than to the 20 the checkpoint happened to recommend.
        assert!(out.get("top_k").is_none(), "{out}");
        // Everything the file is also for.
        assert_eq!(out["eos_token_id"], serde_json::json!([151645, 151643]));
        assert_eq!(out["pad_token_id"], serde_json::json!(151654));
        assert_eq!(out["bos_token_id"], serde_json::json!(151643));
        assert_eq!(out["max_length"], serde_json::json!(40960));
        assert_eq!(out["transformers_version"], serde_json::json!("5.5.4"));

        // And this is what FreeToken's loader would make of the result.
        assert_eq!(
            Sampling::from_config(out.as_object().unwrap()),
            Sampling { temperature: Some(1.0), top_k: None, top_p: Some(0.8) }
        );

        revert(&dir).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join(CONFIG_FILE)).unwrap(), QWEN);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
