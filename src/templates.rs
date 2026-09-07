//! Chat template overrides.
//!
//! FreeToken has no `--chat-template` flag: it loads the prompt template through
//! `AutoTokenizer.from_pretrained(model_path)`, so the only way to change it is to change
//! what that call finds. On the transformers release FreeToken pins, a
//! `chat_template.jinja` in the model directory takes precedence over the `chat_template`
//! key inside `tokenizer_config.json`, which makes dropping that one file in the correct
//! and least invasive override.
//!
//! That means applying a template *writes into the checkpoint directory*, so this module
//! is built around making that reversible and obvious:
//!
//! * whatever `chat_template.jinja` was there first is moved aside, never overwritten;
//! * a marker file records what was applied, from where, and whether there was an
//!   original, so [`status`] can report the truth even after ft-man restarts;
//! * [`revert`] puts it back exactly — restoring the original, or removing the file
//!   entirely when the model never had one.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// The file transformers reads. Writing this shadows `tokenizer_config.json`.
pub const TEMPLATE_FILE: &str = "chat_template.jinja";
/// Where the checkpoint's own template is parked while an override is in place.
pub const BACKUP_FILE: &str = "chat_template.jinja.ft-man-original";
/// Records what ft-man applied, so an override survives a restart visibly.
pub const MARKER_FILE: &str = ".ft-man-template.json";

// ---------------------------------------------------------------- the store

/// Where fetched templates are kept, shared across every model.
pub fn store_dir() -> PathBuf {
    crate::config::state_dir().join("templates")
}

/// Provenance for a stored template, written beside it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TemplateMeta {
    /// Hugging Face repo it came from, when it was fetched rather than imported.
    pub source: Option<String>,
    /// The commit the fetch resolved to, so "which version is this" is answerable.
    pub revision: Option<String>,
    /// Path within that repo.
    pub repo_path: Option<String>,
    pub fetched_at: Option<String>,
    /// `template_version` declared inside the jinja, when it declares one.
    pub version: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StoredTemplate {
    pub name: String,
    pub path: PathBuf,
    pub meta: TemplateMeta,
    pub size: u64,
}

impl StoredTemplate {
    pub fn read(&self) -> Result<String> {
        std::fs::read_to_string(&self.path)
            .with_context(|| format!("reading {}", self.path.display()))
    }

    /// One-line description: the declared version if there is one, else the source.
    pub fn subtitle(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(v) = &self.meta.version {
            parts.push(v.clone());
        }
        if let Some(s) = &self.meta.source {
            parts.push(s.clone());
        }
        if parts.is_empty() {
            parts.push("imported".into());
        }
        parts.join("  ·  ")
    }
}

fn meta_path(name: &str) -> PathBuf {
    store_dir().join(format!("{name}.meta.json"))
}

fn template_path(name: &str) -> PathBuf {
    store_dir().join(format!("{name}.jinja"))
}

/// Every stored template, sorted by name.
pub fn list() -> Vec<StoredTemplate> {
    let dir = store_dir();
    let Ok(rd) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut out: Vec<StoredTemplate> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jinja"))
        .filter_map(|path| {
            let name = path.file_stem()?.to_string_lossy().into_owned();
            let size = std::fs::metadata(&path).ok()?.len();
            let meta = std::fs::read_to_string(meta_path(&name))
                .ok()
                .and_then(|raw| serde_json::from_str(&raw).ok())
                .unwrap_or_default();
            Some(StoredTemplate { name, path, meta, size })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

pub fn get(name: &str) -> Option<StoredTemplate> {
    list().into_iter().find(|t| t.name == name)
}

/// Save a template into the store, filling in the declared version.
pub fn save(name: &str, jinja: &str, mut meta: TemplateMeta) -> Result<StoredTemplate> {
    anyhow::ensure!(!name.trim().is_empty(), "a template needs a name");
    if let Some(problem) = validate(jinja) {
        anyhow::bail!("{problem}");
    }
    let name = sanitize_name(name);
    std::fs::create_dir_all(store_dir())
        .with_context(|| format!("creating {}", store_dir().display()))?;
    meta.version = meta.version.or_else(|| extract_version(jinja));
    meta.fetched_at = meta
        .fetched_at
        .or_else(|| Some(chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)));

    let path = template_path(&name);
    crate::config::write_atomic(&path, jinja)?;
    crate::config::write_atomic(&meta_path(&name), &serde_json::to_string_pretty(&meta)?)?;
    let size = jinja.len() as u64;
    Ok(StoredTemplate { name, path, meta, size })
}

pub fn remove(name: &str) -> Result<()> {
    let path = template_path(name);
    std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    let _ = std::fs::remove_file(meta_path(name));
    Ok(())
}

/// Turn a repo path into a store name: `archive/v22.3.2-sharp/chat_template.jinja` in
/// repo `org/Qwen-Sharp-Chat-Templates` becomes `Qwen-Sharp-Chat-Templates-v22.3.2-sharp`,
/// while the repo-root template keeps the bare repo name.
pub fn name_for(repo: &str, repo_path: &str) -> String {
    let repo_base = repo.rsplit('/').next().unwrap_or(repo);
    let parent = Path::new(repo_path).parent().and_then(|p| p.file_name());
    match parent {
        Some(dir) => sanitize_name(&format!("{repo_base}-{}", dir.to_string_lossy())),
        None => sanitize_name(repo_base),
    }
}

/// Keep names safe as single filename components.
pub fn sanitize_name(name: &str) -> String {
    let cleaned: String = name
        .trim()
        .chars()
        .map(|c| if c.is_alphanumeric() || "-_.".contains(c) { c } else { '-' })
        .collect();
    let cleaned = cleaned.trim_matches(['-', '.']).to_string();
    if cleaned.is_empty() {
        "template".into()
    } else {
        cleaned
    }
}

/// Pull `template_version` out of a jinja file. This repo's templates declare one, and it
/// is far more meaningful than a file name.
pub fn extract_version(jinja: &str) -> Option<String> {
    let idx = jinja.find("template_version")?;
    let rest = &jinja[idx..];
    let start = rest.find(['"', '\''])?;
    let quote = rest.as_bytes()[start] as char;
    let after = &rest[start + 1..];
    let end = after.find(quote)?;
    let value = after[..end].trim();
    (!value.is_empty() && value.len() < 128).then(|| value.to_string())
}

/// A cheap structural check. It cannot prove a template renders — that needs the real
/// tokenizer, which [`preflight_command`] arranges — but it catches an empty file, an
/// HTML error page saved by mistake, or plain text with no jinja in it at all.
pub fn validate(jinja: &str) -> Option<String> {
    let trimmed = jinja.trim();
    if trimmed.is_empty() {
        return Some("the template is empty".into());
    }
    if trimmed.starts_with('<') && trimmed.to_lowercase().contains("<html") {
        return Some("that looks like an HTML page, not a jinja template".into());
    }
    if !trimmed.contains("{%") && !trimmed.contains("{{") {
        return Some("no jinja tags found; this does not look like a chat template".into());
    }
    let opens = trimmed.matches("{%").count();
    let closes = trimmed.matches("%}").count();
    if opens != closes {
        return Some(format!("unbalanced jinja tags: {opens} '{{%' but {closes} '%}}'"));
    }
    None
}

// ---------------------------------------------------------------- per model

/// What ft-man recorded when it applied a template to a checkpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppliedTemplate {
    pub name: String,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub revision: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    pub applied_at: String,
    /// Whether the checkpoint had its own `chat_template.jinja` before this.
    pub had_original: bool,
}

#[derive(Debug, Clone)]
pub enum Status {
    /// The checkpoint's own template, however it ships it.
    BuiltIn,
    /// ft-man applied an override.
    Overridden(Box<AppliedTemplate>),
    /// A `chat_template.jinja` exists that ft-man did not write — someone edited the
    /// checkpoint by hand. Reported rather than silently overwritten.
    Foreign,
}

impl Status {
    pub fn is_overridden(&self) -> bool {
        matches!(self, Status::Overridden(_))
    }

    pub fn label(&self) -> String {
        match self {
            Status::BuiltIn => "built-in".into(),
            Status::Foreign => "custom (not applied by ft-man)".into(),
            Status::Overridden(a) => match &a.version {
                Some(v) => format!("{} ({v})", a.name),
                None => a.name.clone(),
            },
        }
    }
}

/// What template a checkpoint directory will actually serve.
pub fn status(model_dir: &Path) -> Status {
    let marker = model_dir.join(MARKER_FILE);
    if let Ok(raw) = std::fs::read_to_string(&marker) {
        if let Ok(applied) = serde_json::from_str::<AppliedTemplate>(&raw) {
            // Trust the marker only while the file it describes is still there; someone
            // may have deleted it by hand.
            if model_dir.join(TEMPLATE_FILE).is_file() {
                return Status::Overridden(Box::new(applied));
            }
        }
    }
    if model_dir.join(TEMPLATE_FILE).is_file() {
        return Status::Foreign;
    }
    Status::BuiltIn
}

/// Apply a template to a checkpoint directory.
///
/// Re-applying over an existing ft-man override does not re-back-up: the first backup is
/// the checkpoint's genuine original, and clobbering it with a previous override would
/// make [`revert`] restore the wrong thing.
pub fn apply(model_dir: &Path, template: &StoredTemplate, jinja: &str) -> Result<()> {
    anyhow::ensure!(model_dir.is_dir(), "{} is not a directory", model_dir.display());
    if let Some(problem) = validate(jinja) {
        anyhow::bail!("{problem}");
    }

    let target = model_dir.join(TEMPLATE_FILE);
    let backup = model_dir.join(BACKUP_FILE);
    let previous = status(model_dir);

    let had_original = match &previous {
        Status::Overridden(a) => a.had_original,
        _ => {
            if target.is_file() {
                std::fs::rename(&target, &backup).with_context(|| {
                    format!("backing up the checkpoint's own template to {}", backup.display())
                })?;
                true
            } else {
                false
            }
        }
    };

    crate::config::write_atomic(&target, jinja)
        .with_context(|| format!("writing {}", target.display()))?;

    let applied = AppliedTemplate {
        name: template.name.clone(),
        source: template.meta.source.clone(),
        revision: template.meta.revision.clone(),
        version: template.meta.version.clone().or_else(|| extract_version(jinja)),
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
        anyhow::bail!("no ft-man template override is in place here");
    };
    let target = model_dir.join(TEMPLATE_FILE);
    let backup = model_dir.join(BACKUP_FILE);

    if applied.had_original {
        anyhow::ensure!(
            backup.is_file(),
            "the checkpoint's original template is missing from {}",
            backup.display()
        );
        std::fs::rename(&backup, &target)
            .with_context(|| format!("restoring {}", target.display()))?;
    } else {
        // The checkpoint never had one; leaving an empty file behind would still shadow
        // tokenizer_config.json, so it has to go.
        if target.is_file() {
            std::fs::remove_file(&target)
                .with_context(|| format!("removing {}", target.display()))?;
        }
    }
    let _ = std::fs::remove_file(model_dir.join(MARKER_FILE));
    Ok(())
}

/// Every directory an override should be written to for one model.
///
/// A checkpoint and the FTW build converted from it are the same model, and `ft
/// checkpoint` copies the tokenizer files into its output — so a template applied to only
/// one of them would silently not apply to whichever the engine is actually pointed at.
pub fn targets(model: &crate::models::Model) -> Vec<PathBuf> {
    let mut out = vec![model.path.clone()];
    if let Some(ftw) = &model.converted_to {
        out.push(ftw.clone());
    }
    out
}

/// True when a path lies inside a Hugging Face hub cache snapshot.
///
/// Recognized by the `models--org--name/snapshots` shape rather than by comparing against
/// the configured cache root: a second cache, a relocated `HF_HOME`, or a shared dataset
/// bind-mounted at a different path in each container must all be caught, and the layout
/// is the only thing they have in common.
///
/// Writing there is allowed, and warned about. It was briefly refused outright, which was
/// wrong: `ft checkpoint` converts HF safetensors, so a GGUF checkpoint has no FTW build to
/// redirect the override to, and the refusal made the Templates tab permanently unusable
/// for the exact models people download. The write itself is safe — the checkpoint's own
/// `chat_template.jinja` is a symlink into `blobs/`, and renaming a link aside leaves the
/// blob untouched, so `u` still restores it. What the reader has to be told is that the
/// directory is shared: other tools reading this cache will see the override too, and a
/// later `hf download` of the repo may replace it.
pub fn is_hub_cache_path(dir: &Path) -> bool {
    let names: Vec<&str> = dir.components().filter_map(|c| c.as_os_str().to_str()).collect();
    names.windows(2).any(|w| w[0].starts_with("models--") && w[1] == "snapshots")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ft-man-tpl-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn stored(name: &str) -> StoredTemplate {
        StoredTemplate {
            name: name.into(),
            path: PathBuf::from("/dev/null"),
            meta: TemplateMeta {
                source: Some("org/repo".into()),
                version: Some("v22.4.1".into()),
                ..Default::default()
            },
            size: 0,
        }
    }

    const JINJA: &str = "{%- set template_version = \"qwen3.8-froggeric-v22.4.1\" %}\n{{ x }}";

    #[test]
    fn the_declared_version_is_extracted() {
        assert_eq!(extract_version(JINJA).as_deref(), Some("qwen3.8-froggeric-v22.4.1"));
        assert_eq!(extract_version("{%- set template_version = 'v1' %}").as_deref(), Some("v1"));
        assert_eq!(extract_version("{{ nothing }}"), None);
    }

    #[test]
    fn validation_rejects_things_that_are_not_templates() {
        assert!(validate("").is_some());
        assert!(validate("   ").is_some());
        assert!(validate("<!DOCTYPE html><html><body>404</body></html>").is_some());
        assert!(validate("just some prose with no tags").is_some());
        // A truncated tag is caught by the balance check.
        assert!(validate("{% if x %").is_some());
        assert!(validate(JINJA).is_none());
        assert!(validate("{{ messages }}").is_none());

        // What this check deliberately does NOT do: an unclosed `if` is tag-balanced and
        // only a real jinja parser would notice. That is what the render preflight is
        // for, so the cheap check must not pretend to cover it.
        assert!(validate("{% if x %} body").is_none());
    }

    #[test]
    fn names_are_derived_from_the_repo_and_path() {
        assert_eq!(
            name_for("peculiar-ragdoll/Qwen-Sharp-Chat-Templates", "chat_template.jinja"),
            "Qwen-Sharp-Chat-Templates"
        );
        assert_eq!(
            name_for(
                "peculiar-ragdoll/Qwen-Sharp-Chat-Templates",
                "archive/v22.3.2-sharp/chat_template.jinja"
            ),
            "Qwen-Sharp-Chat-Templates-v22.3.2-sharp"
        );
    }

    #[test]
    fn names_stay_safe_as_filenames() {
        assert_eq!(sanitize_name("../../etc/passwd"), "etc-passwd");
        assert_eq!(sanitize_name("a b/c"), "a-b-c");
        assert_eq!(sanitize_name("  ...  "), "template");
        assert!(!sanitize_name("../x").contains('/'));
    }

    #[test]
    fn applying_over_a_checkpoints_own_template_is_reversible() {
        let dir = tmpdir("own");
        std::fs::write(dir.join(TEMPLATE_FILE), "ORIGINAL {{ x }}").unwrap();
        assert!(matches!(status(&dir), Status::Foreign));

        apply(&dir, &stored("sharp"), JINJA).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join(TEMPLATE_FILE)).unwrap(), JINJA);
        assert_eq!(std::fs::read_to_string(dir.join(BACKUP_FILE)).unwrap(), "ORIGINAL {{ x }}");
        let Status::Overridden(a) = status(&dir) else { panic!("should be overridden") };
        assert_eq!(a.name, "sharp");
        assert_eq!(a.version.as_deref(), Some("v22.4.1"));
        assert!(a.had_original);

        revert(&dir).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join(TEMPLATE_FILE)).unwrap(), "ORIGINAL {{ x }}");
        assert!(!dir.join(BACKUP_FILE).exists());
        assert!(!dir.join(MARKER_FILE).exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn applying_to_a_checkpoint_without_one_removes_the_file_on_revert() {
        let dir = tmpdir("none");
        assert!(matches!(status(&dir), Status::BuiltIn));

        apply(&dir, &stored("sharp"), JINJA).unwrap();
        assert!(dir.join(TEMPLATE_FILE).is_file());
        assert!(!dir.join(BACKUP_FILE).exists(), "there was nothing to back up");

        revert(&dir).unwrap();
        // Leaving even an empty file would keep shadowing tokenizer_config.json.
        assert!(!dir.join(TEMPLATE_FILE).exists());
        assert!(matches!(status(&dir), Status::BuiltIn));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn re_applying_preserves_the_checkpoints_genuine_original() {
        let dir = tmpdir("re");
        std::fs::write(dir.join(TEMPLATE_FILE), "ORIGINAL {{ x }}").unwrap();

        apply(&dir, &stored("first"), "{{ first }}").unwrap();
        apply(&dir, &stored("second"), "{{ second }}").unwrap();
        // The backup must still be the checkpoint's own, not the first override.
        assert_eq!(std::fs::read_to_string(dir.join(BACKUP_FILE)).unwrap(), "ORIGINAL {{ x }}");

        revert(&dir).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join(TEMPLATE_FILE)).unwrap(), "ORIGINAL {{ x }}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_marker_without_its_file_is_not_trusted() {
        let dir = tmpdir("orphan");
        apply(&dir, &stored("sharp"), JINJA).unwrap();
        std::fs::remove_file(dir.join(TEMPLATE_FILE)).unwrap();
        // Someone deleted the template by hand: report reality, not the marker.
        assert!(matches!(status(&dir), Status::BuiltIn));
        assert!(revert(&dir).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_broken_template_is_refused_before_anything_is_touched() {
        let dir = tmpdir("bad");
        std::fs::write(dir.join(TEMPLATE_FILE), "ORIGINAL {{ x }}").unwrap();
        assert!(apply(&dir, &stored("bad"), "not a template").is_err());
        // The checkpoint is untouched: no backup taken, no marker written.
        assert_eq!(std::fs::read_to_string(dir.join(TEMPLATE_FILE)).unwrap(), "ORIGINAL {{ x }}");
        assert!(!dir.join(BACKUP_FILE).exists());
        assert!(!dir.join(MARKER_FILE).exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reverting_when_nothing_was_applied_is_an_error_not_a_silent_no_op() {
        let dir = tmpdir("norevert");
        assert!(revert(&dir).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_cache_snapshot_is_recognized_wherever_it_is_mounted() {
        assert!(is_hub_cache_path(Path::new(
            "/workspace/huggingface/hub/models--unsloth--Qwen3.8/snapshots/abc123"
        )));
        // Same cache, different mount point in a sibling container.
        assert!(is_hub_cache_path(Path::new(
            "/mnt/shared/hub/models--acme--M/snapshots/deadbeef/nested"
        )));
        assert!(!is_hub_cache_path(Path::new("/workspace/models/unsloth--Qwen3.8-ftw")));
        assert!(!is_hub_cache_path(Path::new("/models/models--not--a--cache")));
    }

    /// The regression behind a crash: [`targets`] briefly filtered cache paths out, so a
    /// GGUF checkpoint downloaded into the cache — with no FTW build, because `ft
    /// checkpoint` converts safetensors and cannot produce one — yielded an empty list. The
    /// UI then confirmed an apply that listed no directories, reported success for zero
    /// writes, and indexed the empty list.
    #[test]
    fn a_cache_checkpoint_is_a_target_so_an_apply_has_somewhere_to_go() {
        let model = crate::models::Model {
            name: "acme/M:Q4_K_M".into(),
            repo: Some("acme/M".into()),
            variant: Some("Q4_K_M".into()),
            path: PathBuf::from("/hf/hub/models--acme--M/snapshots/abc123/Q4_K_M"),
            format: crate::models::Format::Gguf,
            size_bytes: 0,
            arch: None,
            model_type: None,
            is_moe: false,
            num_experts: None,
            num_layers: None,
            quant: None,
            max_position: None,
            ftw_fingerprint: None,
            converted_to: None,
            modified: None,
        };
        let targets = targets(&model);
        assert_eq!(targets.len(), 1, "a cache checkpoint must still be writable: {targets:?}");
        assert!(is_hub_cache_path(&targets[0]), "and the caller must be able to warn about it");
    }

    /// Writing into a snapshot is safe, which is why it is allowed rather than refused: the
    /// checkpoint's own template is a symlink into `blobs/`, and moving a link aside leaves
    /// the blob — shared with every other revision and tool — byte-for-byte intact.
    #[test]
    fn applying_into_a_cache_snapshot_never_touches_the_shared_blob() {
        let dir = tmpdir("cachesnapshot");
        let blobs = dir.join("models--acme--M/blobs");
        let snap = dir.join("models--acme--M/snapshots/abc123");
        std::fs::create_dir_all(&blobs).unwrap();
        std::fs::create_dir_all(&snap).unwrap();
        let blob = blobs.join("deadbeef");
        std::fs::write(&blob, "ORIGINAL {{ x }}").unwrap();
        std::os::unix::fs::symlink("../../blobs/deadbeef", snap.join(TEMPLATE_FILE)).unwrap();

        let tpl = StoredTemplate {
            name: "t".into(),
            path: dir.join("t.jinja"),
            size: 0,
            meta: TemplateMeta::default(),
        };
        apply(&snap, &tpl, "NEW {{ x }}").expect("a cache snapshot must be writable");

        assert_eq!(std::fs::read_to_string(snap.join(TEMPLATE_FILE)).unwrap(), "NEW {{ x }}");
        assert_eq!(
            std::fs::read_to_string(&blob).unwrap(),
            "ORIGINAL {{ x }}",
            "the blob is shared; writing a template must not reach through the link"
        );

        revert(&snap).expect("revert must restore the checkpoint's own template");
        assert_eq!(std::fs::read_to_string(snap.join(TEMPLATE_FILE)).unwrap(), "ORIGINAL {{ x }}");
        assert_eq!(std::fs::read_to_string(&blob).unwrap(), "ORIGINAL {{ x }}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_ftw_build_is_targeted_alongside_its_checkpoint() {
        let mut model = crate::models::Model {
            name: "m".into(),
            repo: None,
            variant: None,
            path: PathBuf::from("/models/m"),
            format: crate::models::Format::Hf,
            size_bytes: 0,
            arch: None,
            model_type: None,
            is_moe: false,
            num_experts: None,
            num_layers: None,
            quant: None,
            max_position: None,
            ftw_fingerprint: None,
            converted_to: None,
            modified: None,
        };
        assert_eq!(targets(&model), vec![PathBuf::from("/models/m")]);

        model.converted_to = Some(PathBuf::from("/models/m-ftw"));
        assert_eq!(
            targets(&model),
            vec![PathBuf::from("/models/m"), PathBuf::from("/models/m-ftw")]
        );
    }
}
