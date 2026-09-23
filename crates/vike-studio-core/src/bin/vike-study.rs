//! `vike-study` — a thin shim over [`vike_studio_core::study_cli::run`].
//!
//! ⚠ The body moved to the library so the `vike` multicall dispatcher can reach the same code
//! without a second static copy of the DataFusion closure. This bin is KEPT rather than deleted:
//! `required-features`, every installed path and every `CARGO_BIN_EXE_*` reference still resolve,
//! and nothing that invokes `vike-study` by name has to change.
//!
//! It takes `std::env::args()` here because a standalone binary IS the process — under the
//! dispatcher the argv arrives as a parameter instead, from the one sweep that process performs.
//!
//! ⚠ The ENVIRONMENT sweep happens here for the same reason and arrived on 2026-09-23, when the
//! study started routing its history: `$VIKE_DATAHUB_ADDR` names the peer and `$VIKE_SETTINGS_DIR`
//! chooses the credential store the node pair is read from. Both are handed to `run` as a MAP
//! rather than read inside it, which is what keeps that function's rows out of
//! `crates/vike-ops/tests/settings_registry.rs`'s `LIBRARY_PIN` — the may-only-shrink work-list of
//! libraries reading global state their caller cannot see.

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let env: std::collections::HashMap<String, String> = std::env::vars().collect();
    vike_studio_core::study_cli::run(&env, &args)
}
