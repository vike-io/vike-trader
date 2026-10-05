//! The Clock trait — the backtest=live linchpin (Nautilus's Test/Live clock split).
//! Backtest components read time from a `TestClock` driven by replayed events; live components
//! read the wall clock. Same trait, so the same engine/strategy code runs in both worlds.

use std::cell::Cell;
use std::time::{SystemTime, UNIX_EPOCH};

pub trait Clock {
    /// Current time as epoch milliseconds (UTC) — the unit every ported Python path uses.
    fn now_ms(&self) -> i64;
}

/// Any `Fn() -> i64` is a `Clock`, so test harnesses can inject `Box::new(move || …)`
/// (self-advancing counters, scripted stalls) without a named impl per test.
impl<F: Fn() -> i64> Clock for F {
    fn now_ms(&self) -> i64 {
        self()
    }
}

/// Wall-clock epoch milliseconds (UTC), saturating to 0 before the epoch. The free-function twin
/// of [`LiveClock::now_ms`] for the many call sites that need a wall-clock stamp without threading
/// a `Clock` — consolidates the `SystemTime::now().duration_since(UNIX_EPOCH)…` idiom that was
/// otherwise copy-pasted per venue (and drops the `.unwrap()`-on-clock-skew panic some copies had).
pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Wall-clock epoch milliseconds (UTC) as `u64`, saturating to 0 before the epoch. The unsigned twin
/// of [`now_ms`] for callers that need a `u64` stamp directly — the Hyperliquid `AtomicU64`
/// signing-nonce paths — without an `as u64` cast at every call site. Bit-identical to
/// `now_ms() as u64` for every post-epoch instant (which is always).
pub fn now_ms_u64() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Wall-clock epoch nanoseconds (UTC), saturating to 0 before the epoch. The nanosecond twin of
/// [`now_ms`] (venue receive-stamps, raw-tap `local_ns`).
pub fn now_ns() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as i64).unwrap_or(0)
}

/// Wall-clock epoch MICROSECONDS (UTC), saturating to 0 before the epoch. The microsecond twin of
/// [`now_ms`]/[`now_ns`] — Aster v3 EIP-712 request nonces are µs; consolidates the panicking
/// `.unwrap()` copies the aster adapter's exec/user-data pumps otherwise carried.
pub fn now_us() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_micros() as i64).unwrap_or(0)
}

/// Wall-clock epoch SECONDS (UTC). The seconds twin of [`now_ms`], for the two composition roots
/// that hand `vike_backtest::backtest_cli::run` its timestamp parameter (the shipped `backtest`
/// bin and the multicall's `backtest` arm) — the call that was copy-pasted into both.
///
/// ⚠ UNLIKE every sibling above, a clock set before the epoch answers with a NEGATIVE second
/// rather than saturating to `0`: zero is a real instant, and a failure wearing a valid value is
/// how a run manifest starts lying about when it happened. Do not "simplify" this to
/// `unwrap_or(0)`.
pub fn now_unix_secs() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(before) => -(before.duration().as_secs() as i64),
    }
}

/// Wall-clock time.
#[derive(Debug, Default, Clone, Copy)]
pub struct LiveClock;

impl Clock for LiveClock {
    fn now_ms(&self) -> i64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
    }
}

/// Simulated time, advanced by the backtest driver as it replays events.
#[derive(Debug, Default)]
pub struct TestClock {
    now: Cell<i64>,
}

impl TestClock {
    pub fn new(start_ms: i64) -> Self {
        TestClock { now: Cell::new(start_ms) }
    }
    /// Advance (or set) simulated time. The driver calls this with each event's timestamp.
    pub fn set_ms(&self, ms: i64) {
        self.now.set(ms);
    }
}

impl Clock for TestClock {
    fn now_ms(&self) -> i64 {
        self.now.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_ms_u64_tracks_now_ms() {
        // The u64 twin reads the same wall clock as the canonical i64 `now_ms`, just unsigned. Both
        // are post-epoch and read microseconds apart, so they agree to within a wide slack.
        let ms_i64 = now_ms();
        let ms_u64 = now_ms_u64();
        assert!(ms_i64 > 0, "now_ms should be post-epoch");
        assert!(ms_u64 > 0, "now_ms_u64 should be post-epoch");
        assert!(
            (ms_u64 as i64 - ms_i64).abs() < 60_000,
            "u64 twin ({ms_u64}) must track the i64 reading ({ms_i64})"
        );
    }

    #[test]
    fn now_unix_secs_tracks_now_ms() {
        // The seconds twin reads the same wall clock as `now_ms`, truncated to whole seconds. Both are
        // post-epoch and read microseconds apart, so they agree to within a wide slack.
        let secs = now_unix_secs();
        let ms = now_ms();
        assert!(secs > 0, "now_unix_secs should be post-epoch");
        assert!((ms / 1000 - secs).abs() < 60, "seconds twin ({secs}) must track now_ms ({ms})");
    }
}
