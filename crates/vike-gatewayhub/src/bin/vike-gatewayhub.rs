//! `vike-gatewayhub` — a thin shim over [`vike_gatewayhub::gatewayhub_cli::run`].
//!
//! The body lives in the library so the `vike-backend` multicall dispatcher can reach the same code
//! without a second static copy of the venue stack. The bin is KEPT rather than left to the
//! multicall alone, as the datahub's is: every `CARGO_BIN_EXE_*` reference (the crate's own
//! `tests/help_cli.rs` among them) resolves it.
//!
//! This file is the composition root, and it holds the two things a library may not — the ONE
//! `std::env::vars()` sweep and the ONE `current_dir()`.

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect(); // run() takes the TAIL — the dispatcher passes one too
    let vars: std::collections::HashMap<String, String> = std::env::vars().collect();
    let cwd = std::env::current_dir().ok();
    vike_gatewayhub::gatewayhub_cli::run(&vars, cwd.as_deref(), &args)
}
