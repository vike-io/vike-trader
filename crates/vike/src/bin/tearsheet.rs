//! `tearsheet` — a thin shim over [`vike_backend::tearsheet_cli::run`].
//!
//! ⚠ The body lives in this package's library so the `vike-backend` multicall dispatcher reaches the
//! same code. This bin is KEPT rather than deleted: it is the process
//! `crates/vike/tests/tearsheet_cli.rs` spawns (`CARGO_BIN_EXE_tearsheet`), and nothing invoking
//! `tearsheet` by name has to change.
//!
//! ⚠ It moved here from `vike-report` on 2026-10-11 with the CLI, and it is now the one explicit
//! `[[bin]]` of this package (`crates/vike/Cargo.toml`, `required-features = ["tearsheet"]`): the
//! module it calls exists only under that feature. It is NOT shipped — the release builds
//! `--bin vike-backend` alone, and the multicall's `report` row reaches the same function.
//!
//! The environment sweep happens HERE because a standalone binary IS the process. Under the
//! dispatcher the map arrives as a parameter instead, from the one sweep that process performs —
//! which is why `run` takes it rather than reading it.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let env: std::collections::HashMap<String, String> = std::env::vars().collect();
    vike_backend::tearsheet_cli::run(&env, &args)
}
