//! **The workspace's ONE test polling loop.** `test-support`-gated, so no shipped build compiles it.
//!
//! [`wait_until`] asks a condition until it holds or a deadline passes and answers whether it held.
//! A test that waits for another thread (a node folding, a socket answering, a pump draining) waits
//! through it with the timeout its own call site names; a site that must fail loudly calls
//! [`wait_for`], which asserts the answer as `timed out waiting for <what>` (about twenty files had
//! carried that one-line assert as a wrapper of their own), or asserts with a message of its own.
//! About fifty copies of this loop, with two signatures and five poll intervals, were folded here.
//!
//! It lives here for the reason `capture` does: this crate is a leaf every such test reaches with a
//! dev edge. The crates below it are no home: `vike-util`, `vike-paths` and `vike-model` sit in
//! `crates/vike-ops/tests/architecture/clock_pin.rs`'s `DETERMINISM_CRITICAL_CRATES`, where a
//! feature-gated module's `Instant` read counts as a library clock read, and `vike-marketdata` is
//! published to crates.io.

use std::time::{Duration, Instant};

/// The sleep between two looks at the condition.
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Ask `cond` until it holds or `within` has passed; whether it held. `cond` is asked at least once,
/// and once more after the sleep that crosses the deadline, so a condition that became true during
/// that sleep still counts.
pub fn wait_until(within: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + within;
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// [`wait_until`], asserted: panics `timed out waiting for <what>` when `cond` has not held within
/// `within`. The timeout is the call site's (each suite names its own bound); `what` names what never
/// happened.
pub fn wait_for(within: Duration, what: &str, cond: impl FnMut() -> bool) {
    assert!(wait_until(within, cond), "timed out waiting for {what}");
}

#[cfg(test)]
#[path = "poll_tests.rs"]
mod poll_tests;
