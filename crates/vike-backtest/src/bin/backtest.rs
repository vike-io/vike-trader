//! `backtest` — a thin shim over [`vike_backtest::backtest_cli::run`].
//!
//! ⚠ The body moved to the library so the `vike` multicall dispatcher reaches the same code without
//! a second static copy of DataFusion. The bin is KEPT: every installed path and every
//! `CARGO_BIN_EXE_*` reference still resolves.
//!
//! This file is the composition root now, and it holds the two things a library may not:
//!
//! * the ONE `std::env::vars()` sweep — the settings registry's rule;
//! * the ambient CLOCK. `crates/vike-ops/tests/clock_pin.rs`'s `CLOCK_PIN` keeps
//!   `std::time::SystemTime::now()` out of the library tree, so `run` takes the timestamp as a
//!   parameter and this is where the real clock is read.
//!
//! ⚠ A clock set before the epoch answers with a NEGATIVE second rather than `0`: zero is a real
//! instant, and a failure wearing a valid value is how a manifest starts lying about when a run
//! happened. That behaviour moved here with the function; do not "simplify" it to `unwrap_or(0)`.

use std::process::ExitCode;

fn now_unix_secs() -> i64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(before) => -(before.duration().as_secs() as i64),
    }
}

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
    // Inert for every other consumer — `arg`/`has_flag`/`parse_addr_flag`/`run_rm_series`/
    // `run_fetch` all scan for tokens and index relative to their own hit. It has no compile-time
    // guard either: `crates/vike-backtest/tests/optimizer_cli.rs`'s
    // `the_profile_is_positional_and_the_flag_still_works` is the only thing that catches its loss.
    let args: Vec<String> = std::env::args().skip(1).collect();
    let vars: std::collections::HashMap<String, String> = std::env::vars().collect();
    // ⚠ `None` STUDIO TABLE, and it is a layer fact rather than an omission (ruling 7). Under
    // `--addr` this daemon serves the four profile-shaped/roster COMPUTE verbs and refuses the
    // three STUDIO ones by name, because their runners live in `vike-studio-core` — ABOVE this
    // crate in the layer graph, so no binary of THIS crate can name them. `vike-backend backtest
    // --addr` is the composition root that can, and it passes `Some(...)`.
    vike_backtest::backtest_cli::run(&vars, &args, &now_unix_secs, None)
}
