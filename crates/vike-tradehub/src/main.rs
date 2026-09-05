//! `vike-tradehub` — a thin shim over [`vike_tradehub::tradehub_cli::run`].
//!
//! ⚠ The body moved to the library so the `vike` multicall dispatcher reaches the same code without
//! a second static copy of its closure. The bin is KEPT and this is the name
//! `deploy/sbin/vike-trader-ci-deploy` installs onto the live daemon, so nothing about a deployment
//! changes.
//!
//! This file is the composition root and holds what a library may not — the ONE
//! `std::env::vars()` sweep and the ONE `current_dir()`. Both are handed to `run`, which seeds them
//! into the process-wide `PROCESS_ENV` through `resolve_settings` rather than sweeping again.

use std::process::ExitCode;

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect(); // run() takes the TAIL — the dispatcher passes one too
    let env: std::collections::HashMap<String, String> = std::env::vars().collect();
    let cwd = std::env::current_dir().ok();
    vike_tradehub::tradehub_cli::run(&env, cwd.as_deref(), &argv)
}
