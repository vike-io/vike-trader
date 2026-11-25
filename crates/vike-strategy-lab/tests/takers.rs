//! The shipped TAKER strategy files (plan 2b's C1: every file of its section 6 but the makers and
//! the gallery), compiled in this crate's library (`vike_strategy_lab::takers`) and checked
//! three ways:
//!
//! * its OWN tests, copied into the file beside the code they test (each file's `#[cfg(test)]`
//!   block, run by the library's unit tests; `crate::test_support` is `src/test_support.rs`);
//! * its declared defaults against what its reader lands on for an empty table ([`headers`]); the
//!   words a mount of it says, pinned by the daemon before plan 2b ([`mount_lines`]);
//! * the engine-driven tests of the library twin it was copied from, re-pointed at the file
//!   (`grid_dca_engine`, `funding_capture_engine`, `sport_taker_engine`, `cheap_np_strategy`,
//!   `momentum_harness`).

// Named at this root so each check reads a file as `super::<file>`.
use vike_strategy_lab::takers::{
    buy_hold, cheap_catch_updown_fair_value, covered_call, dca_accumulate, funding_capture,
    funding_carry, grid, momentum, pairs_zscore, sport_copy_follower, trailing_scalper,
};

// The doubles the checks name (`super::test_support`); the library's own copy is `#[cfg(test)]`, so
// this binary compiles the same file. A shared test module, so an `allow`.
#[allow(dead_code, clippy::allow_attributes)]
#[path = "../src/test_support.rs"]
mod test_support;

#[path = "takers/headers.rs"]
mod headers;
#[path = "takers/mount_lines.rs"]
mod mount_lines;

#[path = "takers/cheap_np_strategy.rs"]
mod cheap_np_strategy;
#[path = "takers/funding_capture_engine.rs"]
mod funding_capture_engine;
#[path = "takers/grid_dca_engine.rs"]
mod grid_dca_engine;
#[path = "takers/momentum_harness.rs"]
mod momentum_harness;
#[path = "takers/sport_taker_engine.rs"]
mod sport_taker_engine;
