//! `vike-datahub` — a thin shim over [`vike_datahub::datahub_cli::run`].
//!
//! ⚠ The body moved to the library so the `vike` multicall dispatcher reaches the same code without
//! a second static copy of DataFusion. The bin is KEPT rather than deleted:
//! `deploy/vike-datahub.service`'s `ExecStart` names this file's output, and every
//! `CARGO_BIN_EXE_*` reference still resolves.
//!
//! This file is the composition root now, and it holds the two things a library may not — the ONE
//! `std::env::vars()` sweep and the ONE `current_dir()`. The moved body read the working directory
//! TWICE; it now receives a single answer.

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect(); // run() takes the TAIL — the dispatcher passes one too
    let vars: std::collections::HashMap<String, String> = std::env::vars().collect();
    let cwd = std::env::current_dir().ok();
    vike_datahub::datahub_cli::run(&vars, cwd.as_deref(), &args)
}
