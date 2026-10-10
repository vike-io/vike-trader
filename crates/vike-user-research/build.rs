//! The impure caller of `src/codegen.rs`, which this script `include!`s (the vike-buildinfo
//! precedent: the library's unit tests exercise the exact code the build runs).
//!
//! It resolves the scan root, scans twice — the REAL user_data and the committed fixture tree —
//! and writes both registries into OUT_DIR. Any scan error FAILS THE BUILD with the offending
//! path: a malformed folder is heard at compile time, not as "unknown study" on a run.
//!
//! ## Scan-root resolution (build time, deliberately simpler than the runtime walk)
//!
//! `$VIKE_USER_DATA_DIR` (non-blank) wins outright; otherwise `<workspace-root>/user_data`, where
//! the workspace root is `CARGO_MANIFEST_DIR/../..` — a fixed hop, NOT the runtime marker walk in
//! `crates/vike-model/src/paths/state_path.rs`. A build script runs in a source checkout by
//! construction (the whole limit of the compiled tier — see `user_rust_studies_dir`), so none of
//! the deployment ambiguities that walk arbitrates can occur. A deployment that wants its
//! project-folder studies compiled in sets `VIKE_USER_DATA_DIR` for the build. Identical to
//! `crates/vike-user-strategies/build.rs` on purpose: one override to learn.
//!
//! ## Re-run triggers
//!
//! Identical to the strategy host's. Only the directories the scan READS are watched
//! (`codegen::scanned_dirs`: `research/studies/rust` and `research/studies/rhai`), never the whole
//! user_data, so a write to `runs/`, `logs/` or the strategy tier re-runs nothing here; an ABSENT
//! one is watched through a symlink in OUT_DIR, because cargo calls a path it cannot stat stale on
//! every build. `crates/vike-user-research/src/watch.rs` is the mechanism, and
//! `crates/vike-user-strategies/build.rs`'s module doc carries the lane check that proves it on the
//! real cargo.

use std::time::SystemTime;

include!("src/codegen.rs");

/// The scan root's rerun watch — the same file `lib.rs` compiles as `pub mod watch`. Wrapped in a
/// module because it declares its own imports.
// allow, not expect: the same file is `pub mod watch` in the lib, so the lint fires only on this
// private copy (a build script is its own crate and `lib.rs`'s `#![warn(unreachable_pub)]` never
// reaches it); `expect` would redden any build where it does not fire.
#[allow(unreachable_pub, clippy::allow_attributes)]
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
    // ONLY the directories the scan reads (both tiers), as ABSOLUTE paths (`watch::rerun_directives`
    // says why); the scan keeps its own spelling, so the registry is unchanged.
    let root = Path::new(&manifest_dir).join(&user_data);
    let out = Path::new(&out_dir);
    for line in watch::rerun_directives(&root, &scanned_dirs(&root), out, script_built_at()) {
        println!("{line}");
    }
    write_registry(&user_data, Path::new(&out_dir), "user_registry.rs");

    // The committed fixture tree, scanned UNCONDITIONALLY: it proves the full
    // scan->generate->compile->run pipeline on CI, where the real registry above is empty.
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
            "vike-user-research: refusing to build with malformed study folders:\n  {}",
            outcome.errors.join("\n  ")
        );
    }
    for w in &outcome.warnings {
        println!("cargo:warning=vike-user-research: {w}");
    }
    let rendered = render(&outcome.studies);
    let path = out_dir.join(file);
    std::fs::write(&path, rendered).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}
