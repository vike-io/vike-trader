//! The build script of the compiled user-study host: this host's scan (`src/codegen.rs`, which it
//! `include!`s — the vike-buildinfo precedent, so the library's unit tests exercise the exact code
//! the build runs) handed to the shared mechanism, `vike_model::host_build::driver::run`.
//!
//! The driver resolves the scan root, scans twice — the REAL user_data and the committed fixture
//! tree — and writes both registries into OUT_DIR. Any scan error FAILS THE BUILD with the
//! offending path: a malformed folder is heard at compile time, not as "unknown study" on a run.
//! The scan-root rule (`$VIKE_USER_DATA_DIR` when non-blank, else `<workspace-root>/user_data`)
//! and the re-run triggers are the strategy host's and the mechanism's (`vike_model::host_build`'s
//! module docs): one override to learn. A build script runs in a source checkout by construction
//! (the whole limit of the compiled tier — see `user_rust_studies_dir`); a deployment that wants
//! its project-folder studies compiled in sets `VIKE_USER_DATA_DIR` for the build.
//!
//! What is this host's own: BOTH tiers are watched (`codegen::scanned_dirs`:
//! `research/studies/rust`, which is compiled, and `research/studies/rhai`, listed only for the
//! both-tiers warning), so a write to `runs/`, `logs/` or the strategy tier re-runs nothing here.
//! `crates/vike-model/src/host_build/watch.rs`'s module doc carries the lane check that proves the
//! watch on the real cargo.
//!
//! The environment reads stay HERE, not in the driver: the settings registry keys every env read
//! by `(name, crate)`, and this crate is the one that reads `VIKE_USER_DATA_DIR` at build time.

use vike_model::host_build::driver::{self, Built, Host};

include!("src/codegen.rs");

/// Scan ONE user_data root and render its registry, for the driver.
fn build(root: &Path) -> Built {
    let outcome = scan(root);
    Built { rendered: render(&outcome.studies), errors: outcome.errors, warnings: outcome.warnings }
}

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    let out_dir = std::env::var("OUT_DIR").expect("cargo sets OUT_DIR");

    println!("cargo:rerun-if-env-changed=VIKE_USER_DATA_DIR");
    // `.ok()`: unset and non-UTF-8 both fall to the default; the driver ignores a blank value.
    let override_dir = std::env::var("VIKE_USER_DATA_DIR").ok();

    let host = Host { label: "vike-user-research", noun: "study", scanned_dirs, build };
    driver::run(&host, Path::new(&manifest_dir), Path::new(&out_dir), override_dir.as_deref());
}
