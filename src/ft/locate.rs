//! Finding the FreeToken CLI on this machine.
//!
//! FreeToken installs into a virtualenv, so `ft` is usually *not* on the PATH of the
//! shell that launched ft-man. Rather than making the user configure a path before the
//! tool does anything useful, look in the obvious places and report clearly what was
//! found — the Dashboard shows the resolved path so there is never a mystery about
//! which install is being driven.

use std::path::{Path, PathBuf};

use crate::config::FreetokenCfg;

#[derive(Debug, Clone)]
pub struct Freetoken {
    /// The executable to run. Either an `ft` binary or the Python interpreter.
    pub program: PathBuf,
    /// Arguments that must precede the subcommand (`-m freetoken.cli` for the Python form).
    pub prefix: Vec<String>,
    /// Where it was found, for display.
    pub origin: String,
}

impl Freetoken {
    /// Build the argv for `ft <subcommand> <args...>`.
    pub fn argv(&self, subcommand: &str, args: &[String]) -> Vec<String> {
        let mut argv = self.prefix.clone();
        argv.push(subcommand.to_string());
        argv.extend(args.iter().cloned());
        argv
    }

    /// How the command reads for display, e.g. `ft serve --model ...`.
    pub fn display_program(&self) -> String {
        if self.prefix.is_empty() {
            self.program
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.program.display().to_string())
        } else {
            "ft".into()
        }
    }
}

/// Resolve the FreeToken CLI, preferring explicit configuration over discovery.
///
/// Order: an explicit `binary`, then `<venv>/bin/ft`, then `<venv>/bin/python -m
/// freetoken.cli`, then `ft` on PATH, then a handful of conventional venv locations,
/// then any `python3` that can import `freetoken`.
pub fn resolve(cfg: &FreetokenCfg) -> Result<Freetoken, String> {
    let mut tried: Vec<String> = Vec::new();

    if let Some(bin) = &cfg.binary {
        if is_executable(bin) {
            return Ok(direct(bin.clone(), "config: freetoken.binary"));
        }
        tried.push(format!("{} (config)", bin.display()));
    }

    if let Some(venv) = &cfg.venv {
        let ft = venv.join("bin/ft");
        if is_executable(&ft) {
            return Ok(direct(ft, "config: freetoken.venv"));
        }
        let py = venv.join("bin/python");
        if is_executable(&py) {
            return Ok(via_python(py, "config: freetoken.venv"));
        }
        tried.push(format!("{} (config venv)", venv.display()));
    }

    if let Some(p) = which("ft") {
        return Ok(direct(p, "PATH"));
    }
    tried.push("ft on PATH".into());

    for venv in candidate_venvs() {
        let ft = venv.join("bin/ft");
        if is_executable(&ft) {
            return Ok(direct(ft, "discovered venv"));
        }
    }

    for py in ["python3", "python"] {
        if let Some(p) = which(py) {
            if python_has_freetoken(&p) {
                return Ok(via_python(p, "PATH python"));
            }
        }
    }
    tried.push("python -m freetoken.cli".into());

    Err(format!(
        "could not find the FreeToken CLI (tried: {}). Set freetoken.binary or freetoken.venv in {}.",
        tried.join(", "),
        crate::config::config_path().display()
    ))
}

fn direct(program: PathBuf, origin: &str) -> Freetoken {
    Freetoken { program, prefix: Vec::new(), origin: origin.into() }
}

fn via_python(program: PathBuf, origin: &str) -> Freetoken {
    Freetoken {
        program,
        prefix: vec!["-m".into(), "freetoken.cli".into()],
        origin: format!("{origin} (python -m freetoken.cli)"),
    }
}

/// Conventional places a FreeToken venv ends up on a single-purpose server.
fn candidate_venvs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(v) = std::env::var("VIRTUAL_ENV") {
        out.push(PathBuf::from(v));
    }
    if let Some(home) = dirs::home_dir() {
        for name in [
            ".venv",
            "venv",
            "FreeToken/.venv",
            "freetoken/.venv",
            ".local/share/freetoken/venv",
            ".freetoken/venv",
        ] {
            out.push(home.join(name));
        }
    }
    for base in ["/opt/freetoken", "/opt/FreeToken", "/usr/local/freetoken", "/srv/freetoken"] {
        out.push(PathBuf::from(base));
        out.push(PathBuf::from(base).join(".venv"));
        out.push(PathBuf::from(base).join("venv"));
    }
    out
}

fn python_has_freetoken(python: &Path) -> bool {
    std::process::Command::new(python)
        .args(["-c", "import freetoken"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// A minimal `which`, so the tool has no dependency on the `which` crate for one call.
pub fn which(name: &str) -> Option<PathBuf> {
    if name.contains('/') {
        let p = PathBuf::from(name);
        return is_executable(&p).then_some(p);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(name)).find(|c| is_executable(c))
}

/// Ask the resolved CLI for its version. Cheap: `ft --version` is torch-free.
pub fn probe_version(ft: &Freetoken) -> Option<String> {
    let mut cmd = std::process::Command::new(&ft.program);
    cmd.args(&ft.prefix).arg("--version");
    let out = cmd.output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().next()?.trim();
    (!line.is_empty()).then(|| line.to_string())
}
