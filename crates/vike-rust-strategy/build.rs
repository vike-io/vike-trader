//! Bakes three values in, each via `cargo:rustc-env=`: the toolchain fingerprint into
//! `env!("VIKE_STRATEGY_FINGERPRINT")` (`src/fingerprint.rs` reads it) — identical on both sides of
//! the strategy file boundary because both the host binary and a real compiled strategy file depend on
//! this crate and therefore run this exact function — and the SOURCE HASH of what the cdylib templates
//! compile into `env!("VIKE_STRATEGY_SOURCE_HASH")` (`src/artifact.rs`'s `SOURCE_HASH`;
//! `build/source_hash.rs` says what it covers). A third, the strategy BOUNDARY's hash, goes into
//! `env!("VIKE_STRATEGY_BOUNDARY_HASH")` (`src/sdk.rs`'s `BOUNDARY_HASH`, decision 0296). And it writes the
//! table of this crate's SDK ADAPTERS, `$OUT_DIR/adapters.rs` ([`write_adapters`]), which `src/sdk.rs`'s
//! `built_in_adapters` compiles in: a vike serves the adapters it was built with (decision 0296's amendment
//! of 2026-11-20).
//!
//! ⚠ **This used to ALSO compile the four fixture `.so`s under `tests/fixtures/` here.** A
//! reviewer correctly elevated that to a fix-now defect: this is the build script of a WORKSPACE
//! MEMBER, so `cargo build --workspace`, every `--workspace` clippy invocation and `just
//! windows-check` all paid four nested `cargo build`s neither wanted, and a fixture that failed to
//! compile hard-panicked the ENTIRE workspace build rather than one test. `cargo build`/`clippy`
//! never compile `tests/*.rs` at all (only `cargo test`/`--tests`/`--all-targets` do), so moving
//! the fixture builds into `tests/load_refusals.rs` itself — built lazily, on first use, cached by
//! a content hash under `env!("CARGO_TARGET_TMPDIR")` — confines the cost to the one test binary
//! that actually needs them, exactly as the design's own builder service caches an unchanged
//! source ("an unchanged source makes Run instant").
//!
//! ⚠ **The source hash reads other crates' files, and that is the point of it**: it is what the
//! templates compile, read as text (about 8 MB, a few milliseconds). Paths are relative to this
//! crate's folder, which is where cargo runs a build script. It emits `cargo:rerun-if-changed=` for
//! every path it read, which turns cargo's default package-wide watch OFF: this script, its module
//! and `template/` are named for that reason (the fingerprint reads no file). Run inside a strategy
//! file's own build, against the builder's source tree, it hashes that tree; the value is unused there.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

// `#[path]` is required: this file is a crate root at the PACKAGE root, where a bare `mod` would
// resolve beside it. ⚠ The module reads no environment variable (the settings gate scores a build
// script by its `/build.rs` suffix alone). `allow`, not `expect`: the bytes form (`HashForm::Bytes`) is the
// marker check's (`src/sdk.rs`), never this script's.
#[path = "build/source_hash.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod source_hash;

fn main() {
    let fingerprint = compute_fingerprint();
    println!("cargo:rustc-env=VIKE_STRATEGY_FINGERPRINT={fingerprint}");

    let templates: Vec<String> = source_hash::TEMPLATE_MANIFESTS
        .iter()
        .map(|path| std::fs::read_to_string(path).unwrap_or_default())
        .collect();
    let templates: Vec<&str> = templates.iter().map(String::as_str).collect();
    let hashed =
        source_hash::source_hash(Path::new("../.."), &templates, source_hash::HashForm::Tokens);
    println!("cargo:rustc-env=VIKE_STRATEGY_SOURCE_HASH={}", hashed.hash);
    // Beside the fingerprint: the strategy BOUNDARY's hash (`src/sdk.rs`'s `BOUNDARY_HASH`), over this
    // crate's own C-ABI glue, templates and text goldens (`build/source_hash.rs`'s `boundary_hash`).
    let boundary = source_hash::boundary_hash(Path::new("."), source_hash::HashForm::Tokens);
    println!("cargo:rustc-env=VIKE_STRATEGY_BOUNDARY_HASH={}", boundary.hash);
    // `tests/boundary` by name too: the walk watches only paths that exist, and a golden folder that
    // appears later must still re-run this script (it is committed, so it exists wherever this runs).
    for own in ["build.rs", "build", "template", "tests/boundary"] {
        println!("cargo:rerun-if-changed={own}");
    }
    for path in hashed.watched.iter().chain(&boundary.watched) {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    write_adapters();
}

/// Write `$OUT_DIR/adapters.rs`, the expression `src/sdk.rs`'s `COMPILED_IN` is: one `(folder, marker,
/// face)` row per `adapters/sdk-N/` holding an `ADAPTER` marker, each file compiled in by `include_str!` of
/// its absolute path (the face `None` when it has none, which `built_in_adapters` reports), sorted. The
/// folder is watched UNCONDITIONALLY, as `tests/boundary` is: it always exists (its `README.md` is tracked, and
/// the builder's glue copies it, `crates/vike-strategy-builder/src/render.rs`'s `GLUE_PARTS`), so an adapter
/// added after a warm build re-runs this script and is served, never left out of `built_in_adapters`.
fn write_adapters() {
    let out_dir =
        PathBuf::from(env::var("OUT_DIR").expect("cargo sets OUT_DIR for a build script"));
    let here = env::current_dir().expect("a build script runs in its crate's folder");
    let mut folders: Vec<PathBuf> = fs::read_dir("adapters")
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| here.join("adapters").join(entry.file_name()))
        .filter(|folder| folder.join("ADAPTER").is_file())
        .collect();
    folders.sort();
    let literal = |path: PathBuf| format!("include_str!({:?})", path.display().to_string());
    let mut table = String::from("&[\n");
    for folder in folders {
        let name = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let face = folder.join("face.rs");
        let face = if face.is_file() { format!("Some({})", literal(face)) } else { "None".into() };
        table += &format!("    ({name:?}, {}, {face}),\n", literal(folder.join("ADAPTER")));
    }
    table += "]\n";
    fs::write(out_dir.join("adapters.rs"), table).expect("writing the adapter table");
    println!("cargo:rerun-if-changed=adapters");
}

/// `rustc release+commit-hash; target; profile; opt-level` — deterministic for a given toolchain,
/// target and Cargo invocation, and identical on both sides of the strategy file boundary because both
/// the host binary and a real compiled strategy file depend on this crate and therefore run this exact
/// function.
fn compute_fingerprint() -> String {
    let target = env::var("TARGET").unwrap_or_else(|_| "unknown-target".to_string());
    let profile = env::var("PROFILE").unwrap_or_else(|_| "unknown-profile".to_string());
    let opt_level = env::var("OPT_LEVEL").unwrap_or_else(|_| "?".to_string());
    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let (release, commit_hash) = rustc_version(&rustc);
    format!("rustc={release}+{commit_hash};target={target};profile={profile};opt={opt_level}")
}

fn rustc_version(rustc: &str) -> (String, String) {
    let output = Command::new(rustc).arg("-Vv").output();
    let text = match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => return ("unknown-release".to_string(), "unknown-commit".to_string()),
    };
    let mut release = "unknown-release".to_string();
    let mut commit_hash = "unknown-commit".to_string();
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("release: ") {
            release = v.trim().to_string();
        }
        if let Some(v) = line.strip_prefix("commit-hash: ") {
            commit_hash = v.trim().to_string();
        }
    }
    (release, commit_hash)
}
