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
    let args: Vec<String> = std::env::args().collect();
    let vars: std::collections::HashMap<String, String> = std::env::vars().collect();
    vike_backtest::backtest_cli::run(&vars, &args, &now_unix_secs)
}
