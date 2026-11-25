//! The Clock trait — the backtest=live linchpin (the Test/Live clock split).
//! Backtest components read time from a `ClockTest` driven by replayed events; live components
//! read the wall clock. Same trait, so the same engine/strategy code runs in both worlds.
//!
//! [`WALL_CLOCK_READERS`] names every item of this module that reads the wall clock, and a test
//! here derives that set from this file's own text, so a clock function added here and missing from
//! the list fails this crate's suite instead of letting a list elsewhere fall behind.

use std::cell::Cell;
use std::time::{SystemTime, UNIX_EPOCH};

/// **Every item of this module that reads the wall clock**: its free functions and [`ClockLive`], by
/// name. The ONE list of this crate's clock names, for a reader that must recognise a wall-clock read
/// by name (a saved-run answer may not be given to a strategy that reads one; a ratchet keeps the
/// reads out of the deterministic crates). Such a reader keeps the standard library's and other
/// crates' spellings itself; this list holds only this module's own.
///
/// `the_wall_clock_readers_are_exactly_the_items_that_read_the_clock` derives the set from this
/// file's text: an item whose body reads the system clock and is not named here goes red.
pub const WALL_CLOCK_READERS: [&str; 7] =
    ["now_ms", "now_ms_u64", "now_ns", "now_us", "now_unix_secs", "now_secs", "ClockLive"];

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
/// of [`ClockLive::now_ms`] for the many call sites that need a wall-clock stamp without threading
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
/// that hand `vike_backtesthub::backtest_cli::run` its timestamp parameter (the shipped `backtest`
/// bin and the multicall's `backtesthub` arm) — the call that was copy-pasted into both.
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

/// Wall-clock epoch SECONDS (UTC), saturating to `0` before the epoch — the seconds twin of
/// [`now_ms`], with that family's saturation. For the stamps a venue or a settings row carries in
/// whole seconds: an L2/L1 auth timestamp, an OAuth token's `expires_at`, an `updated_utc` column.
///
/// ⚠ NOT [`now_unix_secs`], which answers a pre-epoch clock with a NEGATIVE second on purpose (a
/// run manifest must not wear a valid-looking `0`). The callers routed here all saturated to `0`
/// and still do; the two differ ONLY on a clock set before 1970. A second count past `i64::MAX`
/// saturates to `i64::MAX` rather than wrapping, as the `updated_utc` copies did.
pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Wall-clock time.
#[derive(Debug, Default, Clone, Copy)]
pub struct ClockLive;

impl Clock for ClockLive {
    fn now_ms(&self) -> i64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
    }
}

/// Simulated time, advanced by the backtest driver as it replays events.
#[derive(Debug, Default)]
pub struct ClockTest {
    now: Cell<i64>,
}

impl ClockTest {
    pub fn new(start_ms: i64) -> Self {
        ClockTest { now: Cell::new(start_ms) }
    }
    /// Advance (or set) simulated time. The driver calls this with each event's timestamp.
    pub fn set_ms(&self, ms: i64) {
        self.now.set(ms);
    }
}

impl Clock for ClockTest {
    fn now_ms(&self) -> i64 {
        self.now.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`WALL_CLOCK_READERS`] is exactly the set of this file's items whose body reads the system
    /// clock: every left-margin `pub fn` and every `impl Clock for` whose text, comments stripped,
    /// names the system clock's type. Read off this file itself, so a reader added here without its
    /// name, or a name left behind by a reader removed, goes red.
    #[test]
    fn the_wall_clock_readers_are_exactly_the_items_that_read_the_clock() {
        let source = crate::scan::strip_comments(include_str!("clock.rs"));
        // The production text: everything above this test module.
        let production = source.split("#[cfg(test)]").next().unwrap_or_default();
        let mut found = std::collections::BTreeSet::new();
        let mut item: Option<&str> = None;
        for line in production.lines() {
            let opened =
                line.strip_prefix("pub fn ").or_else(|| line.strip_prefix("impl Clock for "));
            if let Some(rest) = opened {
                item = rest.split(|c: char| !(c.is_alphanumeric() || c == '_')).next();
            } else if line.starts_with(|c: char| !c.is_whitespace() && c != '}') {
                // Any other item at the left margin ends the one before it.
                item = None;
            }
            if let Some(name) = item
                && line.contains("SystemTime")
            {
                found.insert(name);
            }
        }
        let listed: std::collections::BTreeSet<&str> = WALL_CLOCK_READERS.into_iter().collect();
        assert_eq!(listed.len(), WALL_CLOCK_READERS.len(), "a name is listed twice");
        assert!(found.len() >= 6, "the scan found only {found:?}: it reads nothing");
        assert_eq!(found, listed, "the items that read the wall clock, against the list");
    }

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

    #[test]
    fn now_secs_tracks_now_ms_and_now_unix_secs() {
        // The saturating seconds twin reads the same wall clock; post-epoch it agrees with the
        // non-saturating `now_unix_secs` and with `now_ms / 1000` (the copy `vike-cli`'s record plan
        // spelled), to within a wide slack for the microseconds between the reads.
        let secs = now_secs();
        assert!(secs > 1_577_836_800, "now_secs should be after 2020-01-01: {secs}");
        assert!((now_unix_secs() - secs).abs() < 60);
        assert!((now_ms() / 1000 - secs).abs() < 60);
    }
}
