//! Bakes the toolchain fingerprint into `env!("VIKE_PLUGIN_FINGERPRINT")` (`src/fingerprint.rs`
//! reads it) — identical on both sides of the plugin boundary because both the host binary and a
//! real compiled plugin depend on this crate and therefore run this exact function.
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

use std::env;
use std::process::Command;

fn main() {
    let fingerprint = compute_fingerprint();
    println!("cargo:rustc-env=VIKE_PLUGIN_FINGERPRINT={fingerprint}");
}

/// `rustc release+commit-hash; target; profile; opt-level` — deterministic for a given toolchain,
/// target and Cargo invocation, and identical on both sides of the plugin boundary because both
/// the host binary and a real compiled plugin depend on this crate and therefore run this exact
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
