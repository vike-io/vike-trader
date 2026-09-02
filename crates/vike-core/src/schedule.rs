//! The live-runtime schedule registry (`LiveSchedule`) — the wall-clock twin of the backtest
//! bar-boundary `Schedule` (`vike-backtest/src/schedule.rs`, QC-style `Schedule.On`), so a strategy
//! can "recalibrate A-S at 00:00 UTC" or run "Deribit 08:00-expiry" actions with IDENTICAL semantics
//! in both paths.
//!
//! **The rule itself lives one layer down.** [`TimeRule`] is a PURE, allocation-free, clock-free
//! value type with TWO consumers, so it moved to [`vike_model::schedule`] (verbatim) and is
//! re-exported here — every `vike_core::TimeRule` / `crate::schedule::TimeRule` call site is
//! unchanged, and vike-backtest now reaches the shared rule through vike-model instead of depending
//! on this crate. The live runtime fires it off the [`crate::timer_wheel::DeadlineTimerWheel`] at the
//! drain-loop boundary; vike-backtest drops the SAME `TimeRule` into its `DateRule::Time` variant,
//! checked per bar. One rule, one law: [`TimeRule::last_instant_at_or_before`] is the single crossing
//! primitive both paths call, so a rule computes byte-identical fire INSTANTS whether it is driven by
//! bar timestamps (backtest) or by the injected boundary clock (live).
//!
//! What stays HERE is the registry below: it is this runtime's per-mount table (`mount_schedules`),
//! and the runtime is its only consumer.
//!
//! **Not the hot path.** The live registry is consulted only at the drain-loop boundary (never per
//! market message), so the `p99 < 10µs` fold is untouched — see
//! `runtime::strategy_drive::drive_schedule`.

pub use vike_model::schedule::TimeRule;

/// The live-runtime registry of `(TimeRule, tag)` pairs for ONE mount — the wall-clock twin of the
/// backtest `Schedule`. Each entry latches the last instant it fired so a rule fires AT MOST ONCE
/// per scheduled instant; a long idle gap that skipped several instants coalesces to a single fire
/// of the most recent one (a wall-clock recalibration should not burst-catch-up).
#[derive(Debug, Default)]
pub struct LiveSchedule {
    /// (rule, tag, last-fired instant) — `None` until the first [`TimeRule::crossed`] establishes
    /// the latch (see that method for the establish-then-fire law).
    rules: Vec<(TimeRule, String, Option<i64>)>,
}

impl LiveSchedule {
    pub fn new() -> Self {
        LiveSchedule::default()
    }

    /// Register `tag` to fire (via `Strategy::on_schedule`) whenever `rule` crosses an instant.
    pub fn on(&mut self, rule: TimeRule, tag: &str) {
        self.rules.push((rule, tag.to_string(), None));
    }

    /// No rules registered — the byte-identical gate (an empty schedule arms no timer).
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Number of registered rules.
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// The tags due at `now_ms`: for each rule, fire iff [`TimeRule::crossed`] reports a NEW instant
    /// (the shared latch law — the first check establishes without firing; see that method's doc).
    pub fn check_due(&mut self, now_ms: i64) -> Vec<String> {
        let mut due = Vec::new();
        for (rule, tag, last) in &mut self.rules {
            if rule.crossed(now_ms, last) {
                due.push(tag.clone());
            }
        }
        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One millisecond count of a UTC day (86_400_000) — test-local now that the rule arithmetic
    /// itself moved down to `vike_model::schedule`.
    const DAY_MS: i64 = vike_model::MS_PER_DAY;
    // A fixed modern day index so the arithmetic runs against realistic epochs (2026-ish).
    const DAY0: i64 = 20_650 * DAY_MS; // 2026-07-xx 00:00:00 UTC

    #[test]
    fn registry_establishes_then_fires_each_instant_once() {
        let mut s = LiveSchedule::new();
        s.on(TimeRule::daily_at_utc(0, 0), "recalibrate");
        // first check ESTABLISHES the latch (no spurious fire on mount)
        assert!(s.check_due(DAY0).is_empty());
        // same day, later: no fire
        assert!(s.check_due(DAY0 + 3_600_000).is_empty());
        assert!(s.check_due(DAY0 + DAY_MS - 1).is_empty());
        // next midnight: fires
        assert_eq!(s.check_due(DAY0 + DAY_MS), vec!["recalibrate".to_string()]);
        // same day again: no re-fire
        assert!(s.check_due(DAY0 + DAY_MS + 3_600_000).is_empty());
        // following midnight: fires again
        assert_eq!(s.check_due(DAY0 + 2 * DAY_MS), vec!["recalibrate".to_string()]);
    }

    #[test]
    fn missed_instants_coalesce_to_one_fire() {
        let mut s = LiveSchedule::new();
        s.on(TimeRule::daily_at_utc(0, 0), "daily");
        assert!(s.check_due(DAY0).is_empty()); // establish
                                               // jump three days ahead in one poll — a single coalesced fire, not three.
        assert_eq!(s.check_due(DAY0 + 3 * DAY_MS), vec!["daily".to_string()]);
        // and the next day fires again (the latch advanced to DAY0 + 3 days)
        assert_eq!(s.check_due(DAY0 + 4 * DAY_MS), vec!["daily".to_string()]);
    }
}
