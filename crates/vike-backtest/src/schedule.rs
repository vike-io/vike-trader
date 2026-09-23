//! Scheduled strategy callbacks fired at bar boundaries (QC-style Schedule.On).
//! Port of `core/schedule.py` + `analysis/periods.py::period_key`.
//!
//! Rust adaptation: Python registers arbitrary closures; the borrow checker forbids a stored
//! closure that captures the strategy while the strategy is also `&mut` in the engine loop.
//! Rules are registered with a string TAG instead, and the engine fires
//! `Strategy::on_schedule(ctx, tag)` for each due rule — identical firing points/cadence.
//!
//! WALL-CLOCK rules: [`DateRule::Time`] wraps the shared [`vike_model::schedule::TimeRule`] (daily
//! at HH:MM in a fixed-offset tz, market-open/close offsets), so the SAME rule fires at the SAME
//! wall-clock instants here (checked per bar) and in the live runtime (checked at the timer-wheel
//! poll boundary) — one crossing law,
//! [`vike_model::schedule::TimeRule::last_instant_at_or_before`]. The rule type lives in vike-model
//! (and is re-exported by `vike_core::schedule`) precisely so this crate shares it WITHOUT depending
//! on the live core; `vike-core`'s `LiveSchedule` is the live-side registry counterpart of the
//! [`Schedule`] below.

use vike_model::TimeRule;

/// A comparable label for the calendar period containing `ts_ms` (UTC).
/// daily → "2024-03-15" · weekly → "2024-W11" (ISO) · monthly → "2024-03" ·
/// quarterly → "2024-Q1" · yearly → "2024".
///
/// Delegates to the single implementation in [`crate::periods::period_key`] — the two used to be
/// hand-maintained twins (this one chrono-free, that one chrono), a divergence foot-gun. Backtest
/// is off the latency-gated path, so the shared chrono impl is fine here.
pub fn period_key(ts_ms: i64, period: &str) -> String {
    crate::periods::period_key(ts_ms, period)
}

/// Should a scheduled callback fire at this bar?
pub enum DateRule {
    /// Fire on the first bar of each new calendar period.
    PeriodStart { period: String, last_key: Option<String> },
    /// Fire every n bars (`bar_index % n == 0`).
    EveryNBars { n: usize },
    /// Fire on the first bar AT or AFTER each of a wall-clock [`TimeRule`]'s scheduled instants
    /// (daily-at-HH:MM tz, market-open/close offset) — the SAME rule the live runtime fires off its
    /// timer-wheel poll. `last_fired` latches the last instant so it fires once per instant.
    Time { rule: TimeRule, last_fired: Option<i64> },
}

impl DateRule {
    pub fn month_start() -> Self {
        DateRule::PeriodStart { period: "monthly".into(), last_key: None }
    }
    pub fn week_start() -> Self {
        DateRule::PeriodStart { period: "weekly".into(), last_key: None }
    }
    pub fn quarter_start() -> Self {
        DateRule::PeriodStart { period: "quarterly".into(), last_key: None }
    }
    pub fn year_start() -> Self {
        DateRule::PeriodStart { period: "yearly".into(), last_key: None }
    }
    pub fn every_n_bars(n: usize) -> Self {
        assert!(n >= 1, "EveryNBars(n) requires n >= 1");
        DateRule::EveryNBars { n }
    }

    /// Wrap a wall-clock [`TimeRule`] so it fires per bar in the backtest with the SAME semantics
    /// the live runtime gives it — see the module doc.
    pub fn time(rule: TimeRule) -> Self {
        DateRule::Time { rule, last_fired: None }
    }

    fn is_due(&mut self, ts_ms: i64, bar_index: usize) -> bool {
        match self {
            DateRule::PeriodStart { period, last_key } => {
                let key = period_key(ts_ms, period);
                if last_key.as_deref() == Some(key.as_str()) {
                    return false;
                }
                *last_key = Some(key);
                true
            }
            DateRule::EveryNBars { n } => bar_index.is_multiple_of(*n),
            // The ONE crossing law, shared VERBATIM with the live runtime
            // (`vike_core::LiveSchedule::check_due` calls the same `TimeRule::crossed`, from the
            // same vike-model definition): establish on the first bar, then fire once per
            // crossed wall-clock instant — so a rule fires at byte-identical points on both paths.
            DateRule::Time { rule, last_fired } => rule.crossed(ts_ms, last_fired),
        }
    }
}

/// Registry of (rule, tag) pairs, consulted once per bar by the engine.
#[derive(Default)]
pub struct Schedule {
    rules: Vec<(DateRule, String, Option<usize>)>, // (rule, tag, last bar_index fired)
}

impl Schedule {
    pub fn new() -> Self {
        Schedule::default()
    }

    /// Register `tag` to fire (via `Strategy::on_schedule`) whenever `rule` is due.
    pub fn on(&mut self, rule: DateRule, tag: &str) {
        self.rules.push((rule, tag.to_string(), None));
    }

    /// Tags due at this bar, firing each rule at most once per `bar_index`.
    pub fn check_due(&mut self, ts_ms: i64, bar_index: usize) -> Vec<String> {
        let mut due = Vec::new();
        for (rule, tag, last) in &mut self.rules {
            if *last == Some(bar_index) {
                continue;
            }
            if rule.is_due(ts_ms, bar_index) {
                *last = Some(bar_index);
                due.push(tag.clone());
            }
        }
        due
    }
}
