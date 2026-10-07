//! `tearsheet` — a thin shim over [`vike_report::tearsheet_cli::run`].
//!
//! ⚠ The body moved to the library so the `vike-backend` multicall dispatcher reaches the same code.
//! This bin is KEPT rather than deleted: every installed path and every `CARGO_BIN_EXE_*` reference
//! still resolves, and nothing invoking `tearsheet` by name has to change.
//!
//! The environment sweep happens HERE because a standalone binary IS the process. Under the
//! dispatcher the map arrives as a parameter instead, from the one sweep that process performs —
//! which is why `run` takes it rather than reading it.
//!
//! ⚠ The manifest declared this bin `required-features = ["journal"]` — the only reason it had a
//! `[[bin]]` section at all — until 2026-09-28, when the renderer half of the library moved to
//! `vike-analytics` and the feature was deleted. Nothing here is conditional any more, so the bin
//! is auto-discovered again under the same name.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let env: std::collections::HashMap<String, String> = std::env::vars().collect();
    vike_report::tearsheet_cli::run(&env, &args)
}
