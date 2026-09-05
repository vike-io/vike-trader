//! `tearsheet` — a thin shim over [`vike_report::tearsheet_cli::run`].
//!
//! ⚠ The body moved to the library so the `vike` multicall dispatcher reaches the same code. This
//! bin is KEPT rather than deleted: every installed path and every `CARGO_BIN_EXE_*` reference
//! still resolves, and nothing invoking `tearsheet` by name has to change.
//!
//! The environment sweep happens HERE because a standalone binary IS the process. Under the
//! dispatcher the map arrives as a parameter instead, from the one sweep that process performs —
//! which is why `run` takes it rather than reading it.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let env: std::collections::HashMap<String, String> = std::env::vars().collect();
    vike_report::tearsheet_cli::run(&env, &args)
}
