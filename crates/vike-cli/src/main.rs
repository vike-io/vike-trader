//! `vike-cli` binary — a one-line shim over the [`vike_cli`] library dispatcher. All command logic
//! lives in the library (`src/lib.rs` + `cmd/`); keeping the bin thin gives the package a lib target
//! (so `cargo test --doc -p vike-cli` works) and lets the subcommand modules be unit-tested as a lib.

use std::process::ExitCode;

fn main() -> ExitCode {
    // argv[0] is the binary; the rest is `<command> [args…]`.
    vike_cli::run(std::env::args().skip(1))
}
