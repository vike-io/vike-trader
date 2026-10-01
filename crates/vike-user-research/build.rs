//! The impure caller of `src/codegen.rs` (which this script `include!`s — the vike-buildinfo
//! precedent, so the library's unit tests exercise the exact code the build runs).
//!
//! Resolves the scan root, runs the scan twice — the REAL user_data and the committed fixture
//! tree — and writes both generated registries into OUT_DIR. Any scan error FAILS THE BUILD with
//! the offending path: an operator who dropped a malformed folder must hear about it at compile
//! time, not discover an absent study when a run they were waiting on reports "unknown study".
//!
//! ## Scan-root resolution (build time, deliberately simpler than the runtime walk)
//!
//! `$VIKE_USER_DATA_DIR` (non-blank) wins outright; otherwise `<workspace-root>/user_data`, where
//! the workspace root is `CARGO_MANIFEST_DIR/../..` — a fixed hop, NOT the runtime marker walk in
//! `crates/vike-model/src/state_path.rs`. Build scripts run in a source checkout by construction
//! (which is also the whole limit of the compiled tier — see `user_rust_studies_dir`), so the
//! checkout layout is known and none of the deployment ambiguities the runtime walk exists to
//! arbitrate can occur here. A deployment that wants its project-folder studies compiled in sets
//! `VIKE_USER_DATA_DIR` for the build. Identical to `crates/vike-user-strategies/build.rs`, on
//! purpose: an operator who has learned one override has learned both.

include!("src/codegen.rs");

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    let out_dir = std::env::var("OUT_DIR").expect("cargo sets OUT_DIR");

    // Re-run when the override changes or the scanned tree changes. Registering the directory is
    // enough: cargo watches it recursively, and an absent path simply re-runs on appearance.
    println!("cargo:rerun-if-env-changed=VIKE_USER_DATA_DIR");

    let user_data = match std::env::var("VIKE_USER_DATA_DIR") {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ => Path::new(&manifest_dir).join("..").join("..").join("user_data"),
    };
    println!("cargo:rerun-if-changed={}", user_data.display());
    write_registry(&user_data, Path::new(&out_dir), "user_registry.rs");

    // The committed fixture tree: scanned UNCONDITIONALLY so `cargo test -p vike-user-research`
    // proves the full scan->generate->compile->run pipeline on every CI run, where the real
    // registry above is empty (CI checkouts carry no user_data).
    let fixtures = Path::new(&manifest_dir).join("tests").join("fixture_user_data");
    println!("cargo:rerun-if-changed={}", fixtures.display());
    write_registry(&fixtures, Path::new(&out_dir), "fixture_registry.rs");
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
