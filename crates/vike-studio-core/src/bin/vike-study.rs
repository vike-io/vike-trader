//! `vike-study` — a thin shim over [`vike_studio_core::study_cli::run`].
//!
//! ⚠ The body moved to the library so the `vike` multicall dispatcher can reach the same code
//! without a second static copy of the DataFusion closure. This bin is KEPT rather than deleted:
//! `required-features`, every installed path and every `CARGO_BIN_EXE_*` reference still resolve,
//! and nothing that invokes `vike-study` by name has to change.
//!
//! It takes `std::env::args()` here because a standalone binary IS the process — under the
//! dispatcher the argv arrives as a parameter instead, from the one sweep that process performs.

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    vike_studio_core::study_cli::run(&args)
}
