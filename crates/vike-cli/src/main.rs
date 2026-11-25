//! `vike-cli` binary — a one-line shim over the [`vike_cli`] library dispatcher. All command logic
//! lives in the library (`src/lib.rs` + `cmd/`); keeping the bin thin gives the package a lib target
//! (so `cargo test --doc -p vike-cli` works) and lets the subcommand modules be unit-tested as a lib.

use std::process::ExitCode;

fn main() -> ExitCode {
    // argv[0] is the binary; the rest is `<command> [args…]`, read by the library (decision 0155:
    // the verb word is routed there, and every verb's tail is a `clap` struct).
    let mut argv = std::env::args();
    let _program = argv.next();
    let code = vike_cli::run(argv);
    // This binary installs no log subscriber, and as a daemon's `ExecStartPre=` it is the reader most likely to replay a killed
    // writer's journal: stderr is the unit's journal, the one place that recovery can leave its trace.
    if let Some(recovered) = vike_secrets::take_store_recovery() {
        eprintln!("vike-cli: {recovered}");
    }
    code
}
