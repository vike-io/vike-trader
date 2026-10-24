//! The evaluator tests, one child per input kind, over the fixtures they share.

use super::*;
use crate::delivery::FiredAlert;
use crate::rule::AlertRule;

/// ONE evaluation against a FRESH [`RuleState`], for a test about what a single input fires. A
/// test about state CARRIED across calls (latch, cooldown, once, edge) keeps its own
/// `let mut st = RuleState::default();` in sight instead.
fn fire_signal(rule: &AlertRule, sig: &AlertSignal, now_ms: i64) -> Option<FiredAlert> {
    eval_signal_rule(rule, &mut RuleState::default(), sig, now_ms)
}

/// [`fire_signal`] for an [`AlertEvent`].
fn fire_event(rule: &AlertRule, ev: &AlertEvent<'_>, now_ms: i64) -> Option<FiredAlert> {
    eval_event_rule(rule, &mut RuleState::default(), ev, now_ms)
}

/// ⚠ The five fields [`eval_event_rule`] renders, and no more. `side` is `i32` because that
/// is what `FillEvent::side` is on the core side, so the rendered text is byte-identical.
fn fill_event<'a>(venue: &'a str, symbol: &'a str) -> AlertEvent<'a> {
    AlertEvent::Fill { venue, symbol, side: 1, last_qty: 0.5, last_px: 100.0 }
}

fn stale(series: &str, silent_for_ms: Option<i64>, rows: u64) -> AlertSignal {
    AlertSignal::SeriesStale { series: series.to_string(), silent_for_ms, rows }
}

/// #1749's lane as it crosses the boundary into a delivered body: a depth series receiving 0.42
/// items/s against the 10/s its subscription declares, over the recorder's 900 s cadence window,
/// while the same instrument's trade tape, the governor, ran at 20.3/s (the measured rates are
/// `crates/vike-recorder/tests/cadence_alert.rs`'s `BROKEN_DEPTH_PER_S` and
/// `TAPE_WORST_HOUR_PER_S`). The observed rate is UNROUNDED here, so the two-decimal rendering is
/// what a body assertion sees rather than a value that was already short.
fn slow(series: &str) -> AlertSignal {
    AlertSignal::SeriesSlow {
        series: series.to_string(),
        observed_per_s: 0.4166,
        expected_per_s: 10.0,
        window_secs: 900,
        governor: "trade/binance/BTCUSDT.P".to_string(),
        governor_per_s: 20.3,
    }
}

/// The measured 2026-08-05 window, as it crosses the boundary into a delivered body.
///
/// The counts are the ones the the CI box replay produced for the first fully-dark 30 s window
/// (`kind=book/venue=polymarket/group=btc-updown-5m/date=2026-08-05`, window opening 04:23:00Z):
/// ZERO items across the four still-subscribed members, against a rolling baseline of 53,485
/// rows per 30 s taken over the family's own previous twenty windows — while binance wrote 348
/// to 909 trades in every minute of the same span.
fn collapse(family: &str) -> AlertSignal {
    AlertSignal::FamilyCollapse {
        family: family.to_string(),
        observed_items: 0,
        baseline_items: 53_485,
        window_secs: 30,
        members: 4,
        ring_windows: 20,
        licence: "trade/binance/BTCUSDT.P".to_string(),
        licence_items: 435,
    }
}

#[cfg(test)]
mod events;
#[cfg(test)]
mod family;
#[cfg(test)]
mod gate;
/// The recorder-liveness tests — `SeriesStale`, `SeriesSlow` and `FamilyCollapse`, the signal
/// rules a recorder feeds. ⚠ This introduced them as "the vike-free half of this module's tests",
/// run in both builds and the only evaluator tests a standalone lane ran, until 2026-09-28: every
/// test in this module has been vike-free since `core` went on 2026-09-23, there is one build, and
/// that lane was deleted on 2026-09-28.
#[cfg(test)]
mod series;
#[cfg(test)]
mod signals;
#[cfg(test)]
mod snapshot;
