//! Stage the frontend bundle for `rust-embed`.
//!
//! `rust-embed` needs a directory that always exists, and `web/dist` does not: the
//! frontend is built explicitly by whoever produces a binary, and a `cargo build` with no
//! Node installed still has to succeed so the Rust side can be developed and tested. So
//! this copies `web/dist` into `$OUT_DIR/web` when it is there and writes a page
//! explaining how to build it when it is not. It never runs npm — a build script that
//! shells out to a package manager turns every `cargo build` into a network operation.

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is set by cargo"));
    let staged = out.join("web");
    let _ = std::fs::remove_dir_all(&staged);
    std::fs::create_dir_all(&staged).expect("creating the staging directory");

    let dist = PathBuf::from("web/dist");
    // Only when it is there. `rerun-if-changed` on a path that does not exist is how cargo
    // is told "this input may have appeared", so naming an absent `web/dist` unconditionally
    // made every single `cargo build` on a checkout with no frontend rebuild the whole
    // crate — the common case for anyone working on the Rust half.
    if dist.is_dir() {
        println!("cargo:rerun-if-changed=web/dist");
    }
    if dist.join("index.html").is_file() {
        copy_tree(&dist, &staged);
    } else {
        std::fs::write(staged.join("index.html"), PLACEHOLDER).expect("writing the placeholder");
    }
}

fn copy_tree(from: &Path, to: &Path) {
    let Ok(entries) = std::fs::read_dir(from) else { return };
    for entry in entries.flatten() {
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if src.is_dir() {
            std::fs::create_dir_all(&dst).expect("creating a staging subdirectory");
            copy_tree(&src, &dst);
        } else if let Err(e) = std::fs::copy(&src, &dst) {
            // A file that vanished mid-copy means the frontend is being rebuilt right
            // now; the next cargo build picks it up, and failing here would only turn a
            // race into a broken build.
            println!("cargo:warning=skipping {}: {e}", src.display());
        }
    }
}

const PLACEHOLDER: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>ft-man — frontend not built</title>
<style>
  :root { color-scheme: light dark; }
  body { margin: 0; display: grid; place-items: center; min-height: 100vh;
         font: 15px/1.6 ui-sans-serif, system-ui, sans-serif; }
  main { max-width: 42rem; padding: 2rem; }
  h1 { font-size: 1.25rem; margin: 0 0 1rem; }
  pre { padding: .75rem 1rem; border-radius: .375rem; overflow-x: auto;
        background: rgba(127,127,127,.15); }
  p { opacity: .85; }
</style>
</head>
<body>
<main>
  <h1>The ft-man web interface was not built into this binary.</h1>
  <p>The API is running and serving JSON under <code>/api</code>; only the single-page
     application is missing. Build it, then rebuild ft-man:</p>
  <pre>cd web &amp;&amp; npm ci &amp;&amp; npm run build
cargo build --release</pre>
  <p>A release binary never ships this page: the release workflow fails when
     <code>web/dist/index.html</code> is absent.</p>
</main>
</body>
</html>
"#;
