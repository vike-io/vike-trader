//! `vike-recorder` — a thin shim over [`vike_recorder::recorder_cli::run`].
//!
//! ⚠ The body moved to the library so the `vike` multicall dispatcher reaches the same code. The
//! bin is KEPT: `deploy/vike-recorder.service`'s `ExecStart` names this file's output, and every
//! `CARGO_BIN_EXE_*` reference still resolves.
//!
//! This file is the composition root and holds what a library may not — the ONE
//! `std::env::vars()` sweep and the ONE `current_dir()`. The moved body performed TWO environment
//! sweeps at different instants (`main`'s, and a second inside `webhook_targets`); it now receives
//! one map, which is also the map its boot consumed.

fn main() -> std::process::ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect(); // run_main() takes the TAIL — the dispatcher passes one too
    let vars: std::collections::HashMap<String, String> = std::env::vars().collect();
    let cwd = std::env::current_dir().ok();
    vike_recorder::recorder_cli::run_main(&vars, cwd.as_deref(), &argv)
}
