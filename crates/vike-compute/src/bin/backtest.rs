//! `backtest` — a thin shim over [`vike_compute::backtest_cli::run`].
//!
//! ⚠ The body moved to the library so the `vike-backend` multicall dispatcher reaches the same code
//! without a second static copy of DataFusion. The bin is KEPT: every installed path and every
//! `CARGO_BIN_EXE_*` reference still resolves.
//!
//! This file is the composition root now, and it holds the two things a library may not:
//!
//! * the ONE `std::env::vars()` sweep — the settings registry's rule;
//! * the ambient CLOCK. `crates/vike-ops/tests/architecture/clock_pin.rs`'s `CLOCK_PIN` keeps
//!   `std::time::SystemTime::now()` out of the library tree, so `run` takes the timestamp as a
//!   parameter and this is where the real clock is read.
//!
//! ⚠ A clock set before the epoch answers with a NEGATIVE second rather than `0`: zero is a real
//! instant, and a failure wearing a valid value is how a manifest starts lying about when a run
//! happened. That behaviour lives in `vike_model::time::clock::now_unix_secs` now (shared with the
//! multicall's `backtest` arm, which carried its own copy); do not "simplify" it to `unwrap_or(0)`.

use std::process::ExitCode;

fn main() -> ExitCode {
    // ⚠ `skip(1)` — the PROGRAM PATH is not an argument, and the two composition roots must agree.
    //
    // `crates/vike/src/lib.rs`'s `resolve` hands each tool `argv[1..]` (the installed-symlink
    // shape) or `argv[2..]` (`vike-backend backtest …`) — never argv[0]. This shim passed the FULL
    // `std::env::args()`, so `run` saw a leading token under one root and not under the other.
    // Nothing could observe that while every parser in `backtest_cli.rs` matched exact `--` tokens;
    // the POSITIONAL profile (ruling 14) is the first position-sensitive parser in that file, and
    // under the old shape it would have read this executable's own path as the profile here while
    // reading the real one there. Fixed at the SHIM, never by sniffing args[0] downstream.
    //
    // `clap` reads the line `run` is handed (`Program::try_parse_from` takes the arguments WITHOUT
    // the program name), so a leading argv[0] would be read as the profile. It has no compile-time
    // guard either: `crates/vike-compute/tests/optimizer_cli/flags.rs`'s
    // `the_profile_is_positional_and_the_flag_still_works` is the only thing that catches its loss.
    let args: Vec<String> = std::env::args().skip(1).collect();
    let vars: std::collections::HashMap<String, String> = std::env::vars().collect();
    // ⚠ `None` BUILD STAMP, and it is a package fact rather than an omission. Naming the commit
    // this binary was built from means `vike-buildinfo`, and this bin lives IN the `vike-compute`
    // package — so it can only name that crate if the PACKAGE does, which
    // `crates/vike-buildinfo/tests/identity_adoption.rs`'s `WITHOUT_IDENTITY` declares as refused
    // on build-cost grounds. `crates/vike/src/main.rs`'s `backtest_main` is the composition root
    // that CAN answer, and `vike backtest ...` therefore records a commit where this does not.
    //
    // ⚠ The run is still content-ADDRESSED: `crates/vike-backtest/src/run_fingerprint.rs`'s
    // `input_fingerprint` deliberately excludes the build, so a run from this binary is still
    // comparable against one from any other.
    vike_compute::backtest_cli::run(&vars, &args, &vike_model::time::clock::now_unix_secs, None)
}
