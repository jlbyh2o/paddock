//! Local FreeToken checkout status.
//!
//! Reads the git state of the `vendor-freetoken/` directory and compares the local
//! commit against `upstream/main` and `origin/main`. Used to tell the operator whether
//! the build they have is current.

use serde::Serialize;

/// Where `vendor-freetoken/.git` lives relative to this crate's source tree root.
const VENDOR_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/vendor-freetoken");

/// The result of a `git` check on the FreeToken vendor checkout.
#[derive(Debug, Clone, Serialize)]
pub struct FtCheckout {
    /// The upstream remote URL, e.g. `https://github.com/FlashML-org/FreeToken.git`.
    pub upstream: String,
    /// The origin remote URL, e.g. `git@github.com:jlbyh2o/FreeToken.git`.
    pub origin: String,
    /// The commit ft-man sees as its working tree HEAD (short form).
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
}

/// Return `None` when the vendor directory does not look like a git repo.
pub fn check() -> Option<FtCheckout> {
    let upstream = git_get("config remote.upstream.url", VENDOR_DIR)?;
    let origin = git_get("config remote.origin.url", VENDOR_DIR)?;
    let local_sha = git_get("rev-parse --short HEAD", VENDOR_DIR)?;

    // Fetch so the remote tracking refs are current.
    git_run("fetch --quiet upstream", VENDOR_DIR);
    git_run("fetch --quiet origin", VENDOR_DIR);

    let upstream_sha = git_get("rev-parse --short upstream/main", VENDOR_DIR);
    let origin_sha = git_get("rev-parse --short origin/main", VENDOR_DIR);

    let (origin_ahead, origin_behind) = commit_distance("origin/main", VENDOR_DIR);
    let upstream_behind = match &upstream_sha {
        Some(sha) => count_commits(&local_sha, sha, VENDOR_DIR),
        None => 0,
    };

    let dirty = git_get("status --porcelain", VENDOR_DIR).map(|s| !s.trim().is_empty()).unwrap_or(false);

    Some(FtCheckout {
        upstream,
        origin,
        local_sha,
        upstream_sha: upstream_sha.unwrap_or_default(),
        origin_sha: origin_sha.unwrap_or_default(),
        origin_ahead,
        origin_behind,
        upstream_behind,
        dirty,
    })
}

/// Run a git command and return its stdout on success.
fn git_run(cmd: &str, workdir: &str) {
    std::process::Command::new("git")
        .args(cmd.split_whitespace())
        .current_dir(workdir)
        .stderr(std::process::Stdio::null())
        .status()
        .ok();
}

/// Run a git command and return trimmed stdout, or `None` on failure.
fn git_get(cmd: &str, workdir: &str) -> Option<String> {
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
fn commit_distance(against: &str, workdir: &str) -> (usize, usize) {
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
fn count_commits(from: &str, to: &str, workdir: &str) -> usize {
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
