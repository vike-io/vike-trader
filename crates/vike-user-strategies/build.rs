//! The impure caller of `src/codegen.rs` (which this script `include!`s — the vike-buildinfo
//! precedent, so the library's unit tests exercise the exact code the build runs). Its body is
//! `vike_model::host_build::driver::run`, the build kit both compiled user-code hosts share: this
//! file reads the environment, names the host and hands over its scan.
//!
//! The driver resolves the scan root, runs the scan twice — the REAL user_data and the committed
//! fixture tree — and writes both generated registries into OUT_DIR. Any scan error FAILS THE BUILD
//! with the offending path: an operator who dropped a malformed folder must hear about it at
//! compile time, not discover an absent strategy at mount time.
//!
//! ## Scan-root resolution (build time, deliberately simpler than the runtime walk)
//!
//! `$VIKE_USER_DATA_DIR` (non-blank) wins outright; otherwise `<workspace-root>/user_data`, where
//! the workspace root is `CARGO_MANIFEST_DIR/../..` — a fixed hop, NOT the runtime marker walk in
//! `vike-model/src/paths/state_path.rs`. Build scripts run in a source checkout by construction (the
//! tier README's first line), so the checkout layout is known and none of the deployment
//! ambiguities the runtime walk exists to arbitrate can occur here. A deployment that wants its
//! project-folder strategies compiled in sets `VIKE_USER_DATA_DIR` for the build. The variable is
//! read HERE, never in the kit: the settings registry keys every env read by `(name, crate)`.
//!
//! ## Re-run triggers — and the ones that must NOT fire
//!
//! The script re-runs when the override changes, when a directory the scan READS changes
//! (`codegen::scanned_dirs` — the compiled tier, `user_data/strategies/rust`), and when the fixture
//! tree changes. It is NOT told to watch the whole user_data: cargo watches a directory
//! recursively, so a write to `runs/` or `logs/` — by the program, or by a test — would re-run it
//! and recompile every crate above it. And an ABSENT scanned directory (every fresh clone, most CI
//! checkouts) is watched through a symlink in OUT_DIR rather than named to cargo directly: cargo
//! calls a path it cannot stat stale on EVERY build. `crates/vike-model/src/host_build/watch.rs`'s
//! `rerun_path` carries the mechanism and its module doc the cargo source behind both; the tier's
//! appearance (`vike-cli init`, a hand-made folder) still re-runs the scan.
//!
//! ⚠ The unit tests hold a MODEL of cargo's rerun rule, not cargo: the lane check that proves the
//! watch on the REAL cargo (two builds with the fingerprint log on, the second must be `Fresh`) is
//! spelled in `crates/vike-model/src/host_build/watch.rs`'s module doc, its one home.

use vike_model::host_build::driver::{self, Built, Host};

include!("src/codegen.rs");

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    let out_dir = std::env::var("OUT_DIR").expect("cargo sets OUT_DIR");

    println!("cargo:rerun-if-env-changed=VIKE_USER_DATA_DIR");
    // Unset and non-UTF-8 both fall to the default root; the driver also treats blank as unset.
    let override_dir = std::env::var("VIKE_USER_DATA_DIR").ok();

    let host = Host { label: "vike-user-strategies", noun: "strategy", scanned_dirs, build };
    driver::run(&host, Path::new(&manifest_dir), Path::new(&out_dir), override_dir.as_deref());
}

/// Scan ONE user_data root and render its registry — this host's `Host::build`. A strategy host
/// has no warnings: every objection is an error.
fn build(root: &Path) -> Built {
    let outcome = scan(root);
    Built { rendered: render(&outcome.strategies), errors: outcome.errors, warnings: Vec::new() }
}
