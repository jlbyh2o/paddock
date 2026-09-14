//! Local FreeToken checkout status.
//!
//! When FreeToken is built on the machine that serves it rather than installed from a
//! wheel, "are we running the latest?" is a question about a git working tree. This
//! module reads that tree: which commit it is on, how far behind `upstream/main` and
//! `origin/main` it has fallen, whether it is dirty, and whether the compiled kernels
//! are older than the native sources they were built from.
//!
//! The tree is found at run time, not compile time. The binary is built inside a
//! container that mounts this crate at `/src` and is then copied to the server, so a
//! path baked in with `CARGO_MANIFEST_DIR` names a directory that does not exist where
//! it matters.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::Serialize;

use crate::config::FreetokenCfg;
use crate::ft::Freetoken;

/// A development checkout beside this crate's source. Only a workstation has one; it is
/// the last thing tried, and it exists so `cargo run` in the source tree still shows the
/// pane.
const DEV_VENDOR_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/vendor-freetoken");

/// What makes a git repository a FreeToken checkout rather than, say, the dotfiles repo
/// a venv happens to sit inside. Checked on every candidate.
const MARKER: &str = "python/freetoken";

/// The native sources, and the directory their build products land in. Comparing the two
/// is what distinguishes "pulled" from "pulled and rebuilt" — for an editable install the
/// Python half needs no build step, but these do.
const KERNEL_SRC: &str = "python/freetoken/kernel/csrc";
const KERNEL_DIR: &str = "python/freetoken/kernel";

/// How far up from a venv or a program path to look for the enclosing checkout.
const MAX_ASCENT: usize = 4;

/// The result of a `git` check on the FreeToken checkout.
#[derive(Debug, Clone, Serialize)]
pub struct FtCheckout {
    /// The checkout this describes, so the pane can say which tree it read.
    pub path: String,
    /// The upstream remote URL, e.g. `https://github.com/FlashML-org/FreeToken.git`.
    pub upstream: String,
    /// The origin remote URL, e.g. `https://github.com/jlbyh2o/FreeToken.git`.
    pub origin: String,
    /// The commit the working tree is on (short form).
    pub local_sha: String,
    /// The `upstream/main` commit SHA (short form), or empty when fetch is not possible.
    pub upstream_sha: String,
    /// The `origin/main` commit SHA (short form), or empty when fetch is not possible.
    pub origin_sha: String,
    /// How many commits the local branch is ahead of `origin/main`.
    pub origin_ahead: usize,
    /// How many commits the local branch is behind `origin/main`.
    pub origin_behind: usize,
    /// How many commits the local branch is behind `upstream/main`.
    /// Zero means either the branch is up to date with upstream or the counts disagree
    /// and we cannot reliably report a number.
    pub upstream_behind: usize,
    /// Whether the working tree has uncommitted changes.
    pub dirty: bool,
    /// Whether the compiled kernels predate the last commit to touch their sources.
    /// `None` when there is nothing built to compare against — a wheel install, or a
    /// tree that has never been built.
    pub kernels_stale: Option<bool>,
}

/// Find the FreeToken checkout this machine builds from.
///
/// First hit wins:
///  1. `freetoken.checkout` in config.toml — the explicit answer, for a layout none of
///     the guesses below would find;
///  2. the checkout the configured venv sits inside, which is the usual shape: a
///     `.venv` created in the clone, so `~/FreeToken/.venv` gives `~/FreeToken`;
///  3. the checkout the resolved `ft` program sits inside, for a venv named elsewhere;
///  4. the development `vendor-freetoken/` beside this crate's source.
pub fn locate(cfg: &FreetokenCfg, ft: Option<&Freetoken>) -> Option<PathBuf> {
    if let Some(dir) = &cfg.checkout {
        // An explicit path that is wrong should be visible as wrong rather than quietly
        // replaced by a guess, so this arm returns rather than falling through.
        return is_checkout(dir).then(|| dir.clone());
    }
    if let Some(dir) = cfg.venv.as_deref().and_then(ascend_to_checkout) {
        return Some(dir);
    }
    let program = cfg.binary.as_deref().or_else(|| ft.map(|f| f.program.as_path()));
    if let Some(dir) = program.and_then(ascend_to_checkout) {
        return Some(dir);
    }
    let vendor = PathBuf::from(DEV_VENDOR_DIR);
    is_checkout(&vendor).then_some(vendor)
}

/// A git repository that is recognizably FreeToken.
fn is_checkout(dir: &Path) -> bool {
    dir.join(".git").exists() && dir.join(MARKER).exists()
}

/// Walk up from a venv or program path looking for the checkout enclosing it.
fn ascend_to_checkout(start: &Path) -> Option<PathBuf> {
    let mut dir = if start.is_file() { start.parent()? } else { start };
    for _ in 0..MAX_ASCENT {
        if is_checkout(dir) {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
    None
}

/// Read the checkout at `dir`. Blocking: this fetches from the remotes, so callers run
/// it off the UI thread and on a long timer.
///
/// Returns `None` when the directory does not answer as a git repository at all.
pub fn check(dir: &Path) -> Option<FtCheckout> {
    let upstream = git_get("config remote.upstream.url", dir).unwrap_or_default();
    let origin = git_get("config remote.origin.url", dir).unwrap_or_default();
    let local_sha = git_get("rev-parse --short HEAD", dir)?;

    // Fetch so the remote tracking refs are current. Either remote may be absent — a
    // clone of upstream alone has no `upstream`, a clone with no fork has no second
    // remote — and a failed fetch simply leaves that side unreported.
    git_run("fetch --quiet upstream", dir);
    git_run("fetch --quiet origin", dir);

    let upstream_sha = git_get("rev-parse --short upstream/main", dir);
    let origin_sha = git_get("rev-parse --short origin/main", dir);

    let (origin_ahead, origin_behind) = commit_distance("origin/main", dir);
    let upstream_behind = match &upstream_sha {
        Some(sha) => count_commits(&local_sha, sha, dir),
        None => 0,
    };

    let dirty = git_get("status --porcelain", dir).map(|s| !s.trim().is_empty()).unwrap_or(false);

    Some(FtCheckout {
        path: dir.display().to_string(),
        upstream,
        origin,
        local_sha,
        upstream_sha: upstream_sha.unwrap_or_default(),
        origin_sha: origin_sha.unwrap_or_default(),
        origin_ahead,
        origin_behind,
        upstream_behind,
        dirty,
        kernels_stale: kernels_stale(dir),
    })
}

/// Whether the built kernels are older than the last commit that touched their sources.
///
/// Scoped to `csrc/` on purpose: comparing against HEAD instead would call a rebuild for
/// every pull, including the many that only move Python around.
fn kernels_stale(dir: &Path) -> Option<bool> {
    let built = newest_object(&dir.join(KERNEL_DIR))?;
    let touched: u64 = git_get(&format!("log -1 --format=%ct -- {KERNEL_SRC}"), dir)?
        .trim()
        .parse()
        .ok()?;
    Some(touched > built)
}

/// The mtime, in seconds since the epoch, of the most recently built extension module.
fn newest_object(dir: &Path) -> Option<u64> {
    let mut newest = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        if entry.path().extension().is_none_or(|e| e != "so") {
            continue;
        }
        let secs = entry
            .metadata()
            .ok()?
            .modified()
            .ok()?
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_secs();
        newest = Some(newest.map_or(secs, |n: u64| n.max(secs)));
    }
    newest
}

/// Run a git command, discarding its output.
fn git_run(cmd: &str, workdir: &Path) {
    std::process::Command::new("git")
        .args(cmd.split_whitespace())
        .current_dir(workdir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok();
}

/// Run a git command and return trimmed stdout, or `None` on failure.
fn git_get(cmd: &str, workdir: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(cmd.split_whitespace())
        .current_dir(workdir)
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if out.status.success() {
        String::from_utf8(out.stdout).ok().map(|s| s.trim().to_string())
    } else {
        None
    }
}

/// Count how many commits `A` is ahead/behind `B`.
/// Returns (ahead, behind) as usize tuples.
fn commit_distance(against: &str, workdir: &Path) -> (usize, usize) {
    let out = std::process::Command::new("git")
        .args(["rev-list", "--count", "--left-right", "HEAD..."])
        .arg(against)
        .current_dir(workdir)
        .stderr(std::process::Stdio::null())
        .output();
    let out = match out {
        Ok(out) => out,
        Err(_) => return (0, 0),
    };
    let text = match String::from_utf8(out.stdout) {
        Ok(s) => s,
        Err(_) => return (0, 0),
    };
    let parts: Vec<usize> = text
        .trim()
        .split('\n')
        .flat_map(|line| line.split('\t'))
        .filter_map(|n| n.trim().parse().ok())
        .collect();
    if parts.len() >= 2 {
        (parts[0], parts[1])
    } else if parts.len() == 1 {
        // Only one side printed (ahead or behind).
        (parts[0], 0)
    } else {
        (0, 0)
    }
}

/// Count commits between two SHAs: how many commits from `from` to `to`.
fn count_commits(from: &str, to: &str, workdir: &Path) -> usize {
    std::process::Command::new("git")
        .args(["rev-list", "--count", format!("{from}..{to}").as_str()])
        .current_dir(workdir)
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .and_then(|out| {
            String::from_utf8(out.stdout).ok().and_then(|s| s.trim().parse().ok())
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory that looks like a git checkout of FreeToken, without being one.
    fn fake_checkout(root: &Path) {
        std::fs::create_dir_all(root.join(".git")).expect("the .git marker");
        std::fs::create_dir_all(root.join(MARKER)).expect("the python package");
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("ft-man-checkout-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("the scratch directory");
        dir
    }

    /// The shape the servers actually have: `python -m venv .venv` run inside the clone,
    /// so the configured venv is one level below the tree we want to read.
    #[test]
    fn a_venv_inside_the_clone_finds_the_clone() {
        let root = scratch("venv-inside");
        let clone = root.join("FreeToken");
        fake_checkout(&clone);
        let cfg = FreetokenCfg { venv: Some(clone.join(".venv")), ..Default::default() };

        assert_eq!(locate(&cfg, None).as_deref(), Some(clone.as_path()));
        std::fs::remove_dir_all(&root).ok();
    }

    /// `<venv>/bin/ft` is two levels down, and is what `binary` usually names.
    #[test]
    fn the_program_path_finds_the_clone_around_it() {
        let root = scratch("program");
        let clone = root.join("FreeToken");
        fake_checkout(&clone);
        let cfg =
            FreetokenCfg { binary: Some(clone.join(".venv/bin/ft")), ..Default::default() };

        assert_eq!(locate(&cfg, None).as_deref(), Some(clone.as_path()));
        std::fs::remove_dir_all(&root).ok();
    }

    /// The reason ascent looks for more than `.git`: a venv kept in a home directory that
    /// is itself a git repository would otherwise resolve to the dotfiles.
    #[test]
    fn a_git_repo_that_is_not_freetoken_is_not_a_checkout() {
        let root = scratch("dotfiles");
        std::fs::create_dir_all(root.join(".git")).expect("the .git marker");
        let venv = root.join("venvs/ft");
        std::fs::create_dir_all(&venv).expect("the venv");
        let cfg = FreetokenCfg { venv: Some(venv), ..Default::default() };

        // Not `None`: on a development workstation the vendor clone beside this crate is
        // still there to fall back to. What matters is that ascent did not claim the
        // enclosing repository.
        assert_ne!(locate(&cfg, None).as_deref(), Some(root.as_path()));
        std::fs::remove_dir_all(&root).ok();
    }

    /// An explicit `checkout` is the answer, not a hint: a wrong one reports nothing
    /// rather than silently resolving to some other tree.
    #[test]
    fn an_explicit_checkout_does_not_fall_through() {
        let root = scratch("explicit");
        let clone = root.join("FreeToken");
        fake_checkout(&clone);
        let cfg = FreetokenCfg {
            checkout: Some(root.join("nowhere")),
            venv: Some(clone.join(".venv")),
            ..Default::default()
        };

        assert_eq!(locate(&cfg, None), None);
        std::fs::remove_dir_all(&root).ok();
    }
}
