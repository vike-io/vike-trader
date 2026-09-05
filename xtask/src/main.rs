//! The one program that answers "what does CI run".
//!
//! Invoked as `cargo run -p xtask -- <cmd>`, wrapped by `just xtask <cmd>` so nobody types that.
//! ⚠ **There is deliberately no `cargo xtask` alias**, which is the usual convention: an alias lives
//! in `.cargo/config.toml`, `crates/vike-ops/tests/build_config_gate.rs` requires every leaf path in
//! that file to sit under `target.x86_64-pc-windows-msvc`, and cargo does not allow `[alias]` to be
//! target-scoped — so no spelling of the convention passes the gate that protects the Linux boxes
//! holding the credential store. The `justfile` recipe is the entry point instead.
//!
//! Three commands, one consumer each (and no fourth: a command nobody consumes is a surface that
//! rots unobserved, which is the defect this crate was written to remove):
//!
//! | command | consumer | output |
//! |---|---|---|
//! | `crates` | the `justfile`'s `ci_crates :=` | one line, `-p a -p b …` |
//! | `affected` | `.github/workflows/{ci,release}.yml`'s `plan` step | seven `key=value` lines |
//! | `feature-lanes` | a human, and the gates in `crates/vike-ops/tests/` | `key<TAB>trigger,trigger` |
//!
//! `affected` appends its block to `$GITHUB_OUTPUT` when that variable is set, as well as printing
//! it. Writing the file itself — rather than leaving the workflow to redirect stdout — is what keeps
//! the `plan` step a plain `run:` line with no shell plumbing to get wrong, and it means a stray
//! `cargo` line on stdout could never become a job output.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;

use xtask::ci;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("");
    if let Err(e) = run(cmd) {
        eprintln!("xtask {cmd}: {e}");
        std::process::exit(1);
    }
}

/// The repository root. `cargo run` sets the working directory to the workspace root, and
/// `CARGO_MANIFEST_DIR` is this crate's own directory — its parent — so neither depends on where the
/// caller was standing. The env var wins because a `cargo xtask` invoked from a crate directory keeps
/// that directory as the CWD.
fn repo_root() -> PathBuf {
    match std::env::var_os("CARGO_MANIFEST_DIR") {
        Some(dir) => PathBuf::from(dir).join(".."),
        None => std::env::current_dir().expect("a working directory"),
    }
}

fn run(cmd: &str) -> Result<(), String> {
    let root = repo_root();
    let env: BTreeMap<String, String> = std::env::vars().collect();
    match cmd {
        "crates" => {
            let g = ci::graph::load_graph(&root)?;
            let roster = ci::ci_crates(&g.names);
            let args: Vec<String> = roster.iter().map(|c| format!("-p {c}")).collect();
            println!("{}", args.join(" "));
        }
        "affected" => {
            let plan = ci::compute(&env, &root)?;
            // To STDERR: stdout is the machine-readable surface. Without this line a plan is
            // unfalsifiable after the fact — the 40-file sweep of PR #1139 was only diagnosable
            // because someone read the checkout step's log by hand.
            eprintln!("{}", plan.diagnostic);
            let text = plan.render();
            if let Some(dest) = env.get("GITHUB_OUTPUT").filter(|d| !d.is_empty()) {
                let mut fh = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(dest)
                    .map_err(|e| format!("could not open $GITHUB_OUTPUT ({dest}): {e}"))?;
                fh.write_all(text.as_bytes())
                    .map_err(|e| format!("could not write $GITHUB_OUTPUT: {e}"))?;
            }
            print!("{text}");
        }
        "feature-lanes" => {
            for (key, triggers) in ci::tables::FEATURE_SUITES {
                println!("{key}\t{}", triggers.join(","));
            }
        }
        "docs-bins" => {
            // The `bins.json` docs-data release asset (`xtask::docs_bins`): pretty JSON,
            // newline-terminated, to stdout — `release.yml` redirects it beside the four files the
            // vike-ops `docs_data` bin writes.
            let meta = ci::graph::load_metadata(&root)?;
            let bins = xtask::docs_bins::bins_value(&meta)?;
            let mut out = serde_json::to_string_pretty(&bins)
                .map_err(|e| format!("bins.json did not serialize: {e}"))?;
            out.push('\n');
            print!("{out}");
        }
        other => {
            return Err(format!(
                "unknown command {other:?}. Try: crates | affected | feature-lanes | docs-bins"
            ));
        }
    }
    Ok(())
}
