//! The impure caller of `src/codegen.rs` (which this script `include!`s — the vike-buildinfo
//! precedent, so the library's unit tests exercise the exact code the build runs).
//!
//! Resolves the scan root, runs the scan twice — the REAL user_data and the committed fixture
//! tree — and writes both generated registries into OUT_DIR. Any scan error FAILS THE BUILD with
//! the offending path: an operator who dropped a malformed folder must hear about it at compile
//! time, not discover an absent strategy at mount time.
//!
//! ## Scan-root resolution (build time, deliberately simpler than the runtime walk)
//!
//! `$VIKE_USER_DATA_DIR` (non-blank) wins outright; otherwise `<workspace-root>/user_data`, where
//! the workspace root is `CARGO_MANIFEST_DIR/../..` — a fixed hop, NOT the runtime marker walk in
//! `vike-model/src/paths/state_path.rs`. Build scripts run in a source checkout by construction (the
//! tier README's first line), so the checkout layout is known and none of the deployment
//! ambiguities the runtime walk exists to arbitrate can occur here. A deployment that wants its
//! project-folder strategies compiled in sets `VIKE_USER_DATA_DIR` for the build.
//!
//! ## Re-run triggers — and the ones that must NOT fire
//!
//! The script re-runs when the override changes, when a directory the scan READS changes
//! (`codegen::scanned_dirs` — the compiled tier, `user_data/strategies/rust`), and when the fixture
//! tree changes. It is NOT told to watch the whole user_data: cargo watches a directory
//! recursively, so a write to `runs/` or `logs/` — by the program, or by a test — would re-run it
//! and recompile every crate above it. And an ABSENT scanned directory (every fresh clone, most CI
//! checkouts) is watched through a symlink in OUT_DIR rather than named to cargo directly: cargo
//! calls a path it cannot stat stale on EVERY build. `crates/vike-user-strategies/src/watch.rs`
//! carries the mechanism and the cargo source behind both; the tier's appearance (`vike-cli init`,
//! a hand-made folder) still re-runs the scan.
//!
//! ⚠ **The lane check that proves it on the REAL cargo** — the unit tests hold a model of cargo's
//! rule, not cargo. In a Linux checkout, build twice in a row with cargo's fingerprint log on, in
//! each of two states: no `user_data/` at all (or `VIKE_USER_DATA_DIR` naming a path that does not
//! exist), and a `user_data/` holding only `runs/`, with a new file written under `runs/` between
//! the two builds — the CI-runner case:
//!
//! ```text
//! CARGO_LOG=cargo::core::compiler::fingerprint=info cargo build -v -p vike-user-strategies -p vike-user-research
//! ```
//!
//! The SECOND build must report `Fresh` for both crates and carry no `dirty` line for either build
//! script; before this mechanism existed, every build did.

use std::time::SystemTime;

include!("src/codegen.rs");

/// The scan root's rerun watch — the same file `lib.rs` compiles as `pub mod watch`, so the tests
/// hold the directive printed below. Wrapped in a module because it declares its own imports.
mod watch {
    include!("src/watch.rs");
}

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    let out_dir = std::env::var("OUT_DIR").expect("cargo sets OUT_DIR");

    println!("cargo:rerun-if-env-changed=VIKE_USER_DATA_DIR");

    let user_data = match std::env::var("VIKE_USER_DATA_DIR") {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ => Path::new(&manifest_dir).join("..").join("..").join("user_data"),
    };
    // ONLY the directories the scan reads — never the whole user_data — each as an ABSOLUTE path
    // (a relative override resolves against this package, which is the script's working
    // directory): the watch may plant a symlink in OUT_DIR, and a relative link target would
    // resolve against OUT_DIR instead. The scan keeps the spelling it always had, so the generated
    // registry is unchanged.
    let root = Path::new(&manifest_dir).join(&user_data);
    let out = Path::new(&out_dir);
    for line in watch::rerun_directives(&root, &scanned_dirs(&root), out, script_built_at()) {
        println!("{line}");
    }
    write_registry(&user_data, Path::new(&out_dir), "user_registry.rs");

    // The committed fixture tree: scanned UNCONDITIONALLY so `cargo test -p vike-user-strategies`
    // proves the full scan->generate->compile->construct pipeline on every CI run, where the real
    // registry above is empty (CI checkouts carry no user_data).
    let fixtures = Path::new(&manifest_dir).join("tests").join("fixture_user_data");
    println!("cargo:rerun-if-changed={}", fixtures.display());
    write_registry(&fixtures, Path::new(&out_dir), "fixture_registry.rs");
}

/// When this script was BUILT — necessarily before cargo stamped this run's start, which is what
/// makes it a safe `not_after` for the watch (`src/watch.rs` says why one is needed at all).
fn script_built_at() -> SystemTime {
    std::env::current_exe()
        .and_then(|exe| exe.metadata())
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

fn write_registry(root: &Path, out_dir: &Path, file: &str) {
    let outcome = scan(root);
    if !outcome.errors.is_empty() {
        panic!(
            "vike-user-strategies: refusing to build with malformed strategy folders:\n  {}",
            outcome.errors.join("\n  ")
        );
    }
    let rendered = render(&outcome.strategies);
    let path = out_dir.join(file);
    std::fs::write(&path, rendered).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}
