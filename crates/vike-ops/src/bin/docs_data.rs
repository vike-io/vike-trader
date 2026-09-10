//! `docs_data` — write the `docs-data` release assets this crate renders (`rendered_files`'s set:
//! `venues.json`, `stats.json`, `indicators.json`, `templates.json`, `rosters.json`; `bins.json`
//! is xtask's).
//!
//! A thin file-writing shell over `vike_ops::docs_data::rendered_files`, which is where the whole
//! rendering lives (so `crates/vike-ops/tests/docs_data_gate.rs` gates the exact bytes this bin
//! writes, in-process, without spawning anything). Consumed by `.github/workflows/release.yml`'s
//! "Render the docs-data assets" step, which runs it into `target/release` so every file joins
//! `SHA256SUMS` and the upload loop beside every other asset.
//!
//! Usage: `docs_data [OUT_DIR] [GENERATED_FROM]` — two optional positional arguments: the output
//! directory (default `.`), and the commit `stats.json` reports as `generated_from` (default
//! `vike_ops::docs_data::DEFAULT_GENERATED_FROM`; a blank value is refused rather than defaulted).
//! No environment reads, no settings, no network: the inputs are compile-time tables plus that one
//! argument, and the output is one file per entry of that set.
//!
//! ⚠ The commit is an ARGUMENT because it cannot be a compile-time bake and be true — the release
//! workflow's dispatch trigger and its sccache wrapper each break `option_env!("GITHUB_SHA")` on
//! their own. `vike_ops::docs_data::DEFAULT_GENERATED_FROM`'s doc carries both mechanisms. A binary
//! reading its own argv is the sanctioned shape — libraries take configuration as parameters, only
//! binaries read the process environment (root `CLAUDE.md`, "Settings & configuration") — and
//! reading it here rather than in the library is what keeps the settings registry's `LIBRARY_PIN`
//! ratchet out of it.

use std::path::Path;
use std::process::ExitCode;

use vike_ops::docs_data;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let out_dir = args.next().unwrap_or_else(|| ".".to_string());
    let stamp_arg = args.next();
    if args.next().is_some() {
        eprintln!("usage: docs_data [OUT_DIR] [GENERATED_FROM]");
        return ExitCode::from(2);
    }
    let generated_from = match docs_data::generated_from(stamp_arg.as_deref()) {
        Ok(v) => v,
        Err(why) => {
            eprintln!("docs_data: {why}");
            eprintln!("usage: docs_data [OUT_DIR] [GENERATED_FROM]");
            return ExitCode::from(2);
        }
    };
    for (name, contents) in docs_data::rendered_files(generated_from) {
        let path = Path::new(&out_dir).join(name);
        if let Err(e) = std::fs::write(&path, &contents) {
            eprintln!("docs_data: writing {} failed: {e}", path.display());
            return ExitCode::FAILURE;
        }
        println!("wrote {} ({} bytes)", path.display(), contents.len());
    }
    ExitCode::SUCCESS
}
