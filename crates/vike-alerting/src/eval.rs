//! The PURE alert evaluator: `(rule + per-rule state + one input) -> Option<FiredAlert>`. No I/O,
//! no clocks (the caller passes `now_ms`), no delivery (that is [`crate::delivery`]) — so every
//! trigger's fire/no-fire is unit-tested with plain values. [`crate::AlertEngine`] holds the
//! [`RuleState`] map + the sinks and folds these functions over the enabled rules OFF the hot fold.
//!
//! **Feature `core` gates TWO FUNCTIONS, not the module.** [`eval_snapshot_rule`] names
//! `vike_core::CoreSnapshot` and [`eval_event_rule`] names `vike_model::events::Event`; those two —
//! and only those two — are behind the feature. Everything else here (the state, the cooldown gate,
//! the crossing arithmetic, the indicator and signal entry points) names no vike type at all, so it
//! compiles in the crate's DEFAULT vike-free build where the standalone recorder watchdog can reach
//! it. Its output type, [`FiredAlert`], lives in [`crate::delivery`] alongside the sinks that
//! consume it — the one type both halves share, deliberately kept in the vike-free half.
//!
//! Four inputs, one per evaluation entry point (mirrors [`RuleTrigger`]'s grouping):
//! - [`eval_snapshot_rule`] over a `vike_core::CoreSnapshot` — price crossings, drawdown, recon
//!   (feature `core`);
//! - [`eval_indicator_rule`] over an [`IndicatorSample`] the consumer computed;
//! - [`eval_event_rule`] over a typed `vike_model::events::Event` — fills, rejects (feature `core`);
//! - [`eval_signal_rule`] over an [`AlertSignal`] — feed health, breaker trip, resolution, and a
//!   RECORDED SERIES that stopped receiving rows.
//!
//! Each function no-ops (returns `None`) for a rule whose trigger belongs to a different input, so
//! the engine can hand every enabled rule to the matching entry point without pre-partitioning.
//!
//! Edge/latch semantics live HERE (in [`RuleState`]): scalar crossings ([`Compare`]) edge-detect
//! via `last_value`; drawdown/recon LATCH via `latched`; discrete events/signals fire on each
//! occurrence, rate-limited only by the rule's cooldown/once (both applied by [`maybe_fire`]).

#[cfg(feature = "core")]
use vike_core::CoreSnapshot;
#[cfg(feature = "core")]
use vike_model::events::Event;

// A plain `use`, deliberately: [`FiredAlert`] is defined next to the sinks that consume it (the
// vike-free half — see this module's doc) and is this module's output type, so it has to be in
// scope here — but it is NOT re-exported. This line was a `pub use` on the argument that "every
// historical call site spells it `…::alerting::eval::FiredAlert`"; when that claim was finally
// checked, the tree held exactly one occurrence of that path — the sentence making the claim.
// Every consumer, in and out of this crate, reaches the type through the crate's flat vocabulary
// (`vike_alerting::FiredAlert`, re-exported from `delivery` by `lib.rs`).
use super::delivery::FiredAlert;

use super::rule::{AlertRule, Compare, FeedState, RuleTrigger};

/// Per-rule mutable evaluation state, keyed by `AlertRule::id` in [`crate::AlertEngine`].
/// Rebuilt empty whenever the engine is (re)built — a fresh session re-arms every latch.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RuleState {
    /// previous scalar sample (price / indicator value) — for edge (crossing) detection. `None`
    /// until the first sample, so a rule can never fire on its very first observation (no prior to
    /// cross from).
    pub last_value: Option<f64>,
    /// Running session-peak of the DRAWDOWN CURVE — for [`RuleTrigger::Drawdown`]. Named
    /// `peak_equity` historically; the quantity is `vike_core::Portfolio::drawdown_curve`
    /// (configured capital + the daemon's own P&L), never the cross-venue equity total. See the
    /// `Drawdown` arm of [`eval_snapshot_rule`] for why the wallet had to leave this number.
    pub peak_equity: Option<f64>,
    /// the latch for [`RuleTrigger::Drawdown`] / [`RuleTrigger::ReconAlert`]: `true` while the
    /// condition currently holds, so a fire happens only on the rising edge and re-arms on release.
    pub latched: bool,
    /// wall-clock ms of the last fire — the cooldown gate.
    pub last_fired_ms: Option<i64>,
    /// a `once` rule has already fired this session.
    pub fired_once: bool,
}

/// A transient status the off-fold consumer maps producer state onto (never persisted — signals are
/// live edges, not config). The consumer emits one only on a TRANSITION (e.g. a `StreamHealth`
/// `HealthEvent`, a `SpreadMaker` breaker trip, a Polymarket resolution), so signal rules fire on
/// each occurrence with no latch of their own.
#[derive(Debug, Clone, PartialEq)]
pub enum AlertSignal {
    /// A feed changed health. `degraded == true` for a `HealthEvent::Gap`/`Stale`, `false` for
    /// `Live` (recovery).
    Feed { venue: String, degraded: bool },
    /// A maker per-side fill-rate circuit breaker tripped.
    FillRateBreaker { venue: String, symbol: String },
    /// A watched Polymarket market resolved.
    PolymarketResolution { token_id: String },
    /// A recorded SERIES is not receiving rows — see [`RuleTrigger::SeriesStale`].
    ///
    /// The two shapes are kept apart because they are different diagnoses, exactly as the producer
    /// (`vike_recorder::liveness::Silent`) distinguishes them: `silent_for_ms: Some(ms)` is a series
    /// that received rows and then STOPPED (usually a venue-side stream death), `None` is one that
    /// has NEVER received a row (usually a wrong stream name the venue accepted anyway). Collapsing
    /// them into "stale for N ms" would report the second as freshly broken every restart.
    SeriesStale { series: String, silent_for_ms: Option<i64>, rows: u64 },
}

/// One streaming-indicator reading the off-fold consumer computed (over `vike_indicators` /
/// `vike_chart::indicators`) and feeds to [`eval_indicator_rule`]. Keeping the value on the input
/// (not computed here) is what lets this crate evaluate an indicator threshold with no bar/indicator
/// dependency and stay a pure evaluator.
#[derive(Debug, Clone, PartialEq)]
pub struct IndicatorSample {
    pub venue: String,
    pub symbol: String,
    /// vike-indicators registry key (matches [`RuleTrigger::Indicator::indicator`]).
    pub indicator: String,
    /// which output line this reading is for (matches [`RuleTrigger::Indicator::output`]).
    pub output: usize,
    pub value: f64,
}

/// The rule's display label — its `name`, or its `id` when unnamed.
fn label(rule: &AlertRule) -> String {
    if rule.name.is_empty() { rule.id.clone() } else { rule.name.clone() }
}

/// `filter` (an optional scope) matches `val` when it is `None` (unscoped) or equal.
fn filter_matches(filter: &Option<String>, val: &str) -> bool {
    match filter {
        None => true,
        Some(f) => f.as_str() == val,
    }
}

/// [`filter_matches`]' PREFIX twin, for [`RuleTrigger::SeriesStale`] alone — its doc carries the
/// reason an exact series name cannot be the scope on a venue whose symbols rotate.
fn prefix_matches(filter: &Option<String>, val: &str) -> bool {
    match filter {
        None => true,
        Some(p) => val.starts_with(p.as_str()),
    }
}

/// Did `cur` cross `bound` in `op`'s direction, given the previous sample `prev`? Strict edge:
/// `Above` needs `prev <= bound < cur`; `Below` needs `prev >= bound > cur`.
fn crossed(op: Compare, prev: f64, cur: f64, bound: f64) -> bool {
    match op {
        Compare::Above => prev <= bound && cur > bound,
        Compare::Below => prev >= bound && cur < bound,
    }
}

/// Fold a scalar sample into `st`, returning whether it crossed `bound` in `op`'s direction.
/// ALWAYS records `cur` as the new `last_value` (so the next call has a prior), even when it does
/// not fire — that is what makes the crossing edge-accurate.
fn scalar_crossed(op: Compare, st: &mut RuleState, cur: f64, bound: f64) -> bool {
    // No prior ⇒ cannot detect a crossing yet (`is_some_and` is false on `None`).
    let fired = st.last_value.is_some_and(|prev| crossed(op, prev, cur, bound));
    st.last_value = Some(cur);
    fired
}

/// Apply the rule's cooldown/once gate and, if it passes, build the [`FiredAlert`] and record the
/// fire. `title` is the rule label; `body` is the caller's specifics. Returns `None` when the gate
/// suppresses the fire (leaving `st`'s fire bookkeeping untouched).
fn maybe_fire(
    rule: &AlertRule,
    st: &mut RuleState,
    now_ms: i64,
    body: String,
) -> Option<FiredAlert> {
    if rule.once && st.fired_once {
        return None;
    }
    if rule.cooldown_ms > 0
        && let Some(last) = st.last_fired_ms
        && now_ms.saturating_sub(last) < rule.cooldown_ms
    {
        return None;
    }
    st.last_fired_ms = Some(now_ms);
    if rule.once {
        st.fired_once = true;
    }
    Some(FiredAlert {
        rule_id: rule.id.clone(),
        title: label(rule),
        body,
        ts_ms: now_ms,
        targets: rule.targets.clone(),
    })
}

/// Evaluate a snapshot-driven rule ([`RuleTrigger::Price`] / [`Drawdown`](RuleTrigger::Drawdown) /
/// [`ReconAlert`](RuleTrigger::ReconAlert)) against `snap`. Returns `None` for any other trigger.
///
/// Feature `core` — this is one of the two entry points that names a vike type.
#[cfg(feature = "core")]
pub fn eval_snapshot_rule(
    rule: &AlertRule,
    st: &mut RuleState,
    snap: &CoreSnapshot,
    now_ms: i64,
) -> Option<FiredAlert> {
    match &rule.trigger {
        RuleTrigger::Price { venue, symbol, op, level } => {
            // No published mark yet ⇒ nothing to compare (and DON'T seed last_value from a phantom
            // — the first real mark then can't spuriously "cross").
            let cur = snap.last_mark(venue, symbol)?;
            if scalar_crossed(*op, st, cur, *level) {
                maybe_fire(rule, st, now_ms, format!("{venue} {symbol} {op} {level} (mark {cur})"))
            } else {
                None
            }
        }
        RuleTrigger::Drawdown { pct } => {
            // ⚠ `Portfolio::drawdown_curve`, NOT `CoreSnapshot::equity()`. This rule FOLLOWS the
            // core's own latch onto the daemon's own equity curve (configured capital + realized
            // + unrealized P&L) instead of the cross-venue equity TOTAL, which on an
            // `Authoritative` block includes the venue's wallet for the whole account the
            // credentials open. Reading the total meant a third party's withdrawal from a SHARED
            // account fired this alert with no trading behind it, while a real 25% loss on the
            // daemon's own ~9000 of book was 3.6% of a 62647 total and never fired at all (the CI box,
            // 2026-08-17). The latch and the alert now measure the same quantity by construction —
            // `Portfolio::pnl_total`'s fold is pinned bit-identical to the engine-side scalar the
            // latch uses, so an operator can never be told "no drawdown" about a core that just
            // latched itself liquidate-only.
            let cur = snap.portfolio.drawdown_curve();
            let peak = st.peak_equity.map_or(cur, |p| p.max(cur));
            st.peak_equity = Some(peak);
            // `peak > 0.0` guard unchanged in shape, but note what it now excludes: a portfolio
            // with no configured capital base (`capital_base == 0.0` — a wire-built observe
            // portfolio, or a core assembled with `seed_cash: 0.0`) and no profit yet. The core
            // announces that condition on its own side (`sweep_drawdown_latch`'s DISARMED note);
            // firing an alert whose percentage has no denominator would be worse than silence.
            let dd = if peak > 0.0 { (peak - cur) / peak } else { 0.0 };
            let breached = dd >= *pct;
            let rising = breached && !st.latched;
            st.latched = breached; // re-arm once the curve recovers back under the threshold
            if rising {
                maybe_fire(
                    rule,
                    st,
                    now_ms,
                    format!(
                        "own-PnL drawdown {:.2}% >= {:.2}% (peak {peak}, curve {cur}, capital_base {}, own_pnl {})",
                        dd * 100.0,
                        *pct * 100.0,
                        snap.portfolio.capital_base,
                        snap.portfolio.pnl_total()
                    ),
                )
            } else {
                None
            }
        }
        RuleTrigger::ReconAlert { divergence_kind: kind } => {
            let matching = snap.recon.alerts.iter().find(|a| match kind {
                None => true,
                Some(k) => a.kind.as_str() == k.as_str(),
            });
            let present = matching.is_some();
            let rising = present && !st.latched;
            // Capture detail BEFORE flipping the latch (borrow of `matching` ends here).
            let detail = matching
                .map(|a| format!("{} — {} ({} pending)", a.kind, a.detail, a.proposed_event_count));
            st.latched = present;
            match (rising, detail) {
                (true, Some(d)) => maybe_fire(rule, st, now_ms, format!("reconcile alert: {d}")),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Evaluate an [`RuleTrigger::Indicator`] rule against one [`IndicatorSample`]. Returns `None` for
/// any other trigger, or when the sample is for a different instrument/indicator/output.
pub fn eval_indicator_rule(
    rule: &AlertRule,
    st: &mut RuleState,
    sample: &IndicatorSample,
    now_ms: i64,
) -> Option<FiredAlert> {
    let RuleTrigger::Indicator { venue, symbol, indicator, output, params: _, op, threshold } =
        &rule.trigger
    else {
        return None;
    };
    if venue != &sample.venue
        || symbol != &sample.symbol
        || indicator != &sample.indicator
        || *output != sample.output
    {
        return None;
    }
    if scalar_crossed(*op, st, sample.value, *threshold) {
        maybe_fire(
            rule,
            st,
            now_ms,
            format!(
                "{indicator}[{output}] {op} {threshold} on {venue} {symbol} (value {})",
                sample.value
            ),
        )
    } else {
        None
    }
}

/// Evaluate an event-driven rule ([`RuleTrigger::Fill`] / [`OrderRejected`](RuleTrigger::OrderRejected))
/// against one typed `Event`. Returns `None` for any other trigger or a non-matching event.
///
/// Feature `core` — this is one of the two entry points that names a vike type.
#[cfg(feature = "core")]
pub fn eval_event_rule(
    rule: &AlertRule,
    st: &mut RuleState,
    ev: &Event,
    now_ms: i64,
) -> Option<FiredAlert> {
    match (&rule.trigger, ev) {
        (RuleTrigger::Fill { venue, symbol }, Event::Fill(f)) => {
            if filter_matches(venue, f.venue.as_str()) && filter_matches(symbol, f.symbol.as_str())
            {
                maybe_fire(
                    rule,
                    st,
                    now_ms,
                    format!(
                        "fill {} {} side {} qty {} @ {}",
                        f.venue, f.symbol, f.side, f.last_qty, f.last_px
                    ),
                )
            } else {
                None
            }
        }
        (RuleTrigger::OrderRejected, Event::OrderRejected(e)) => maybe_fire(
            rule,
            st,
            now_ms,
            format!("order {} rejected: {}", e.client_order_id, e.reason),
        ),
        (RuleTrigger::OrderRejected, Event::OrderDenied(e)) => maybe_fire(
            rule,
            st,
            now_ms,
            format!("order {} denied: {}", e.client_order_id, e.reason),
        ),
        _ => None,
    }
}

/// Evaluate a status-signal rule ([`RuleTrigger::Feed`] / [`FillRateBreaker`](RuleTrigger::FillRateBreaker)
/// / [`PolymarketResolution`](RuleTrigger::PolymarketResolution) /
/// [`SeriesStale`](RuleTrigger::SeriesStale)) against one [`AlertSignal`].
/// Returns `None` for any other trigger or a non-matching signal.
pub fn eval_signal_rule(
    rule: &AlertRule,
    st: &mut RuleState,
    sig: &AlertSignal,
    now_ms: i64,
) -> Option<FiredAlert> {
    match (&rule.trigger, sig) {
        (RuleTrigger::Feed { venue, state }, AlertSignal::Feed { venue: sv, degraded }) => {
            let want_degraded = matches!(state, FeedState::Degraded);
            if filter_matches(venue, sv.as_str()) && want_degraded == *degraded {
                let what = if *degraded { "degraded" } else { "recovered" };
                maybe_fire(rule, st, now_ms, format!("feed {sv} {what}"))
            } else {
                None
            }
        }
        (
            RuleTrigger::FillRateBreaker { venue, symbol },
            AlertSignal::FillRateBreaker { venue: sv, symbol: ss },
        ) => {
            if filter_matches(venue, sv.as_str()) && filter_matches(symbol, ss.as_str()) {
                maybe_fire(rule, st, now_ms, format!("fill-rate breaker tripped {sv} {ss}"))
            } else {
                None
            }
        }
        (
            RuleTrigger::PolymarketResolution { token_id },
            AlertSignal::PolymarketResolution { token_id: st_id },
        ) => {
            if filter_matches(token_id, st_id.as_str()) {
                maybe_fire(rule, st, now_ms, format!("polymarket market resolved: {st_id}"))
            } else {
                None
            }
        }
        (
            RuleTrigger::SeriesStale { series_prefix },
            AlertSignal::SeriesStale { series, silent_for_ms, rows },
        ) => {
            if !prefix_matches(series_prefix, series.as_str()) {
                return None;
            }
            let body = match silent_for_ms {
                Some(ms) => format!(
                    "series {series} has STOPPED receiving rows — silent for {}s after {rows} rows",
                    ms / 1_000
                ),
                None => format!(
                    "series {series} has NEVER received a row — check the venue's stream name"
                ),
            };
            maybe_fire(rule, st, now_ms, body)
        }
        _ => None,
    }
}

/// The vike-free half of this module's tests — they run in BOTH builds, and in the
/// `alerting-standalone` lane they are the only evaluator tests that run at all.
#[cfg(test)]
mod stale_tests {
    use super::*;
    use crate::rule::AlertRule;

    fn stale(series: &str, silent_for_ms: Option<i64>, rows: u64) -> AlertSignal {
        AlertSignal::SeriesStale { series: series.to_string(), silent_for_ms, rows }
    }

    /// The 2026-08-05 shape: a series that was writing 250k rows/min stops, and the alert NAMES it.
    /// A venue-scoped `Feed` rule cannot say which series — that is the whole reason this variant
    /// exists.
    #[test]
    fn an_unscoped_rule_fires_for_any_series_and_names_it() {
        let rule = AlertRule::new("s", RuleTrigger::SeriesStale { series_prefix: None });
        let fired = eval_signal_rule(
            &rule,
            &mut RuleState::default(),
            &stale("book/polymarket/0xtok", Some(19 * 60_000), 3_800_000),
            1,
        )
        .expect("an unscoped rule fires for any silent series");
        assert!(fired.body.contains("book/polymarket/0xtok"), "{}", fired.body);
        assert!(
            fired.body.contains("1140s"),
            "the silence duration is the diagnosis: {}",
            fired.body
        );
        assert!(fired.body.contains("3800000"), "…and so is the row count it had: {}", fired.body);
    }

    /// Never-started reads differently from stopped: the second is a venue stream that died, the
    /// first is a subscription that was accepted and never served.
    #[test]
    fn a_never_started_series_reads_as_never_started_not_as_stale_for_zero() {
        let rule = AlertRule::new("s", RuleTrigger::SeriesStale { series_prefix: None });
        let fired = eval_signal_rule(
            &rule,
            &mut RuleState::default(),
            &stale("depth/binance/BTCUSDT", None, 0),
            1,
        )
        .unwrap();
        assert!(fired.body.contains("NEVER received a row"), "{}", fired.body);
        assert!(fired.body.contains("stream name"), "{}", fired.body);
    }

    /// A PREFIX scope, because a Polymarket token id does not exist yet when the rule is written.
    #[test]
    fn the_scope_is_a_prefix_so_a_rotating_family_is_expressible() {
        let rule = AlertRule::new(
            "s",
            RuleTrigger::SeriesStale { series_prefix: Some("book/polymarket/".into()) },
        );
        // a token minted after the rule was written still matches.
        assert!(
            eval_signal_rule(
                &rule,
                &mut RuleState::default(),
                &stale("book/polymarket/9911", Some(600_000), 5),
                1
            )
            .is_some()
        );
        // a different venue does not.
        assert!(
            eval_signal_rule(
                &rule,
                &mut RuleState::default(),
                &stale("trade/binance/BTCUSDT", Some(600_000), 5),
                2
            )
            .is_none()
        );
        // …nor a different KIND of the same venue (the prefix carries the kind too).
        assert!(
            eval_signal_rule(
                &rule,
                &mut RuleState::default(),
                &stale("trade/polymarket/9911", Some(600_000), 5),
                3
            )
            .is_none()
        );
    }

    /// Cross-talk in both directions: another signal must not fire a stale rule, and a stale signal
    /// must not fire another rule.
    #[test]
    fn a_series_stale_signal_and_rule_do_not_cross_with_the_other_kinds() {
        let stale_rule = AlertRule::new("s", RuleTrigger::SeriesStale { series_prefix: None });
        assert!(
            eval_signal_rule(
                &stale_rule,
                &mut RuleState::default(),
                &AlertSignal::Feed { venue: "binance".into(), degraded: true },
                1
            )
            .is_none()
        );

        let feed_rule =
            AlertRule::new("f", RuleTrigger::Feed { venue: None, state: FeedState::Degraded });
        assert!(
            eval_signal_rule(
                &feed_rule,
                &mut RuleState::default(),
                &stale("trade/binance/BTCUSDT", Some(1), 1),
                2
            )
            .is_none()
        );
    }

    /// The rule-level cooldown gates a series-stale rule like any other discrete signal. ⚠ It is
    /// PER RULE, not per series — which is exactly why `vike_recorder::liveness::SilenceWatch` owns
    /// the per-series repeat gate instead of leaning on this one: with six series silent at once, a
    /// rule cooldown would page for the first and swallow the other five.
    #[test]
    fn the_rule_cooldown_is_per_rule_which_is_why_the_producer_gates_per_series() {
        let mut rule = AlertRule::new("s", RuleTrigger::SeriesStale { series_prefix: None });
        rule.cooldown_ms = 60_000;
        let mut st = RuleState::default();
        assert!(eval_signal_rule(&rule, &mut st, &stale("a/v/s", Some(1), 1), 1_000).is_some());
        assert!(
            eval_signal_rule(&rule, &mut st, &stale("b/v/s", Some(1), 1), 1_001).is_none(),
            "a DIFFERENT series is still suppressed by the same rule's cooldown"
        );
    }
}

#[cfg(all(test, feature = "core"))]
mod tests {
    use super::*;
    use crate::rule::AlertRule;
    use vike_core::{ReconAlertView, ReconBlock};
    use vike_model::events::{FillEvent, OrderDenied, OrderRejected};

    fn snap_with_mark(venue: &str, symbol: &str, px: f64) -> CoreSnapshot {
        let mut s = CoreSnapshot::empty(venue, symbol);
        s.marks = vec![(venue.to_string(), symbol.to_string(), px)];
        s
    }

    fn fill_event(venue: &str, symbol: &str) -> Event {
        Event::Fill(FillEvent {
            trade_id: "t1".into(),
            client_order_id: "c1".into(),
            venue: venue.into(),
            symbol: symbol.into(),
            side: 1,
            last_qty: 0.5,
            last_px: 100.0,
            commission: 0.0,
            commission_asset: String::new().into(),
            liquidity_side: "taker".into(),
            ts: 0,
            mark_price: None,
            position_side: "BOTH".into(),
        })
    }

    // ---- Price crossing (edge) --------------------------------------------------------------

    #[test]
    fn price_above_fires_only_on_the_upward_crossing() {
        let rule = AlertRule::new(
            "p",
            RuleTrigger::Price {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                op: Compare::Above,
                level: 100.0,
            },
        );
        let mut st = RuleState::default();
        // First mark below the level: seeds last_value, never fires (no prior to cross from).
        assert!(
            eval_snapshot_rule(&rule, &mut st, &snap_with_mark("binance", "BTCUSDT", 99.0), 1)
                .is_none()
        );
        // Crosses up 99 -> 101: fires once.
        assert!(
            eval_snapshot_rule(&rule, &mut st, &snap_with_mark("binance", "BTCUSDT", 101.0), 2)
                .is_some()
        );
        // Stays above (101 -> 102): must NOT re-fire (edge, not level).
        assert!(
            eval_snapshot_rule(&rule, &mut st, &snap_with_mark("binance", "BTCUSDT", 102.0), 3)
                .is_none()
        );
        // Drops back under then crosses up again: re-arms and fires.
        assert!(
            eval_snapshot_rule(&rule, &mut st, &snap_with_mark("binance", "BTCUSDT", 98.0), 4)
                .is_none()
        );
        assert!(
            eval_snapshot_rule(&rule, &mut st, &snap_with_mark("binance", "BTCUSDT", 105.0), 5)
                .is_some()
        );
    }

    #[test]
    fn price_below_fires_on_the_downward_crossing_and_ignores_the_wrong_symbol() {
        let rule = AlertRule::new(
            "p",
            RuleTrigger::Price {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                op: Compare::Below,
                level: 100.0,
            },
        );
        let mut st = RuleState::default();
        // A mark for a DIFFERENT symbol never matches (no mark for BTCUSDT ⇒ None, no state change).
        assert!(
            eval_snapshot_rule(&rule, &mut st, &snap_with_mark("binance", "ETHUSDT", 1.0), 1)
                .is_none()
        );
        assert_eq!(st.last_value, None, "a non-matching snapshot must not seed last_value");
        // 101 -> 99 crosses down.
        assert!(
            eval_snapshot_rule(&rule, &mut st, &snap_with_mark("binance", "BTCUSDT", 101.0), 2)
                .is_none()
        );
        assert!(
            eval_snapshot_rule(&rule, &mut st, &snap_with_mark("binance", "BTCUSDT", 99.0), 3)
                .is_some()
        );
    }

    // ---- Indicator threshold ----------------------------------------------------------------

    #[test]
    fn indicator_rule_matches_instrument_and_output_then_crosses() {
        let rule = AlertRule::new(
            "i",
            RuleTrigger::Indicator {
                venue: "binance".into(),
                symbol: "ETHUSDT".into(),
                indicator: "rsi".into(),
                output: 0,
                params: vec![14.0],
                op: Compare::Below,
                threshold: 30.0,
            },
        );
        let mut st = RuleState::default();
        let s = |v: f64| IndicatorSample {
            venue: "binance".into(),
            symbol: "ETHUSDT".into(),
            indicator: "rsi".into(),
            output: 0,
            value: v,
        };
        // Wrong indicator / output / symbol never fire and never touch state.
        let wrong = IndicatorSample {
            venue: "binance".into(),
            symbol: "ETHUSDT".into(),
            indicator: "macd".into(),
            output: 0,
            value: 5.0,
        };
        assert!(eval_indicator_rule(&rule, &mut st, &wrong, 1).is_none());
        assert_eq!(st.last_value, None);
        // 40 (seed, no prior) then 25: crosses below 30 → fires.
        assert!(eval_indicator_rule(&rule, &mut st, &s(40.0), 2).is_none());
        assert!(eval_indicator_rule(&rule, &mut st, &s(25.0), 3).is_some());
        // still below → no re-fire.
        assert!(eval_indicator_rule(&rule, &mut st, &s(20.0), 4).is_none());
    }

    // ---- Fill / OrderRejected events --------------------------------------------------------

    #[test]
    fn fill_rule_scoping_fires_only_for_the_matching_instrument() {
        let mut st = RuleState::default();
        let scoped = AlertRule::new(
            "f",
            RuleTrigger::Fill { venue: Some("binance".into()), symbol: Some("BTCUSDT".into()) },
        );
        assert!(eval_event_rule(&scoped, &mut st, &fill_event("binance", "BTCUSDT"), 1).is_some());
        assert!(
            eval_event_rule(&scoped, &mut RuleState::default(), &fill_event("okx", "BTCUSDT"), 2)
                .is_none(),
            "wrong venue must not fire"
        );
        assert!(
            eval_event_rule(
                &scoped,
                &mut RuleState::default(),
                &fill_event("binance", "ETHUSDT"),
                3
            )
            .is_none(),
            "wrong symbol must not fire"
        );
        // Unscoped fires for any fill.
        let any = AlertRule::new("f2", RuleTrigger::Fill { venue: None, symbol: None });
        assert!(
            eval_event_rule(&any, &mut RuleState::default(), &fill_event("okx", "SOLUSDT"), 4)
                .is_some()
        );
        // A non-fill event never fires a fill rule.
        let rej = Event::OrderRejected(OrderRejected {
            client_order_id: "c1".into(),
            reason: "x".into(),
            ts: 0,
        });
        assert!(eval_event_rule(&any, &mut RuleState::default(), &rej, 5).is_none());
    }

    #[test]
    fn order_rejected_rule_fires_on_both_reject_and_deny() {
        let rule = AlertRule::new("r", RuleTrigger::OrderRejected);
        let rej = Event::OrderRejected(OrderRejected {
            client_order_id: "c1".into(),
            reason: "insufficient balance".into(),
            ts: 0,
        });
        let den = Event::OrderDenied(OrderDenied {
            client_order_id: "c2".into(),
            reason: "risk gate".into(),
            ts: 0,
        });
        let fired = eval_event_rule(&rule, &mut RuleState::default(), &rej, 1).unwrap();
        assert!(fired.body.contains("insufficient balance"));
        assert!(eval_event_rule(&rule, &mut RuleState::default(), &den, 2).is_some());
        // an accepted fill never fires an order-rejected rule.
        assert!(
            eval_event_rule(&rule, &mut RuleState::default(), &fill_event("binance", "BTCUSDT"), 3)
                .is_none()
        );
    }

    // ---- Drawdown latch ---------------------------------------------------------------------

    /// A snapshot whose DRAWDOWN CURVE is `curve`, expressed the way a real core does: a fixed
    /// 1000 capital base plus one venue block carrying the daemon's own realized PnL. `equity_total`
    /// is set to a deliberately DIFFERENT, much larger number — the shape a live mount produces,
    /// where the block has adopted a venue wallet — so a rule that reads the wrong field is caught.
    #[cfg(feature = "core")]
    fn dd_snap(curve: f64) -> CoreSnapshot {
        let mut s = CoreSnapshot::empty("binance", "BTCUSDT");
        s.portfolio.capital_base = 1_000.0;
        s.portfolio.venues = vec![vike_core::snapshot::VenueBlock {
            venue: "binance".into(),
            realized_pnl: curve - 1_000.0,
            ..Default::default()
        }];
        s.portfolio.equity_total = 53_647.10600813 + curve;
        s
    }

    #[test]
    fn drawdown_latches_on_breach_and_rearms_after_recovery() {
        let rule = AlertRule::new("d", RuleTrigger::Drawdown { pct: 0.10 });
        let mut st = RuleState::default();
        let snap = dd_snap;
        // Peak climbs to 1000; no drawdown → no fire.
        assert!(eval_snapshot_rule(&rule, &mut st, &snap(1000.0), 1).is_none());
        assert!(eval_snapshot_rule(&rule, &mut st, &snap(1010.0), 2).is_none());
        // Drops to 900 = 10.9% off the 1010 peak → fires once.
        assert!(eval_snapshot_rule(&rule, &mut st, &snap(900.0), 3).is_some());
        // Still down → latched, no re-fire.
        assert!(eval_snapshot_rule(&rule, &mut st, &snap(880.0), 4).is_none());
        // Recovers above the threshold band → re-arms (no fire on recovery).
        assert!(eval_snapshot_rule(&rule, &mut st, &snap(1005.0), 5).is_none());
        assert!(!st.latched, "recovery re-arms the latch");
        // A fresh breach fires again.
        assert!(eval_snapshot_rule(&rule, &mut st, &snap(850.0), 6).is_some());
    }

    /// ⚠ **A THIRD PARTY moving money in a shared venue account must not fire this rule.** The the CI box
    /// shape (2026-08-17): a bybit block that `VIKE_RECONCILE=1` flipped to `Authoritative` by
    /// adopting the SHARED UNIFIED account's 53647.10600813 USDT `walletBalance`, next to paper
    /// mounts. `equity_total` carries that wallet, so a withdrawal collapses it with no trading
    /// behind it at all — and the rule USED to read exactly that field. It now reads
    /// `Portfolio::drawdown_curve`, the same quantity `CoreThread::sweep_drawdown_latch` acts on.
    ///
    /// This test FAILS on the pre-fix evaluator: 31000 is 43% off a 54647 peak, well past `pct`.
    #[test]
    fn a_venue_wallet_movement_with_no_trading_behind_it_never_fires() {
        let rule = AlertRule::new("d", RuleTrigger::Drawdown { pct: 0.10 });
        let mut st = RuleState::default();
        // The daemon's own book is FLAT and stays flat: no position, no fill, no fee, no funding.
        let flat = |wallet: f64| {
            let mut s = CoreSnapshot::empty("binance", "BTCUSDT");
            s.portfolio.capital_base = 1_000.0;
            s.portfolio.venues = vec![
                vike_core::snapshot::VenueBlock { venue: "binance".into(), ..Default::default() },
                vike_core::snapshot::VenueBlock {
                    venue: "bybit".into(),
                    // `balance_mode` deliberately unset (`Delta`, the `Default`): `pnl_total` is
                    // MODE-BLIND — that is exactly the property under test, and it means the fix
                    // does not depend on correctly classifying the block.
                    balance: wallet,
                    equity: wallet,
                    ..Default::default()
                },
            ];
            // what `CoreSnapshot::build`'s aggregate_equity law would produce for those blocks
            s.portfolio.equity_total = 1_000.0 + wallet;
            s
        };
        assert!(eval_snapshot_rule(&rule, &mut st, &flat(53_647.10600813), 1).is_none());
        // ...and now somebody else withdraws 23647 from the shared account.
        assert!(
            eval_snapshot_rule(&rule, &mut st, &flat(30_000.0), 2).is_none(),
            "a wallet this daemon does not own moved; its own PnL did not, so nothing fires"
        );
        assert!(!st.latched, "and the latch was never armed by somebody else's cashflow");
        assert_eq!(
            st.peak_equity,
            Some(1_000.0),
            "the peak is the daemon's own curve (capital base, flat book), never the 54647 total"
        );
    }

    /// The other half, and the one that must NOT be lost in the process: a real loss on the
    /// daemon's OWN book still fires, at the configured fraction of its own capital — even while a
    /// large adopted wallet sits beside it making the loss look like noise in `equity_total`.
    ///
    /// This test also FAILS on the pre-fix evaluator: a 250 loss is 25% of the daemon's 1000 of
    /// book but 0.46% of a 54647 total, so the old reader saw no drawdown worth firing on.
    #[test]
    fn a_real_loss_on_the_daemons_own_book_still_fires_beside_a_large_wallet() {
        let rule = AlertRule::new("d", RuleTrigger::Drawdown { pct: 0.20 });
        let mut st = RuleState::default();
        let book = |own_pnl: f64| {
            let mut s = CoreSnapshot::empty("binance", "BTCUSDT");
            s.portfolio.capital_base = 1_000.0;
            s.portfolio.venues = vec![vike_core::snapshot::VenueBlock {
                venue: "bybit".into(),
                balance: 53_647.10600813,
                unrealized: own_pnl,
                equity: 53_647.10600813 + own_pnl,
                ..Default::default()
            }];
            s.portfolio.equity_total = 53_647.10600813 + own_pnl;
            s
        };
        assert!(eval_snapshot_rule(&rule, &mut st, &book(0.0), 1).is_none(), "flat: peak = 1000");
        let fired = eval_snapshot_rule(&rule, &mut st, &book(-250.0), 2)
            .expect("a 25% loss of the daemon's own capital must fire a 20% drawdown rule");
        assert!(fired.body.contains("own-PnL drawdown 25.00%"), "body: {}", fired.body);
        assert!(fired.body.contains("capital_base 1000"), "body names the base: {}", fired.body);
    }

    // ---- Recon alert ------------------------------------------------------------------------

    #[test]
    fn recon_alert_rule_fires_on_presence_edge_with_optional_kind_filter() {
        let mut with_alert = CoreSnapshot::empty("binance", "BTCUSDT");
        with_alert.recon = ReconBlock {
            alerts: vec![ReconAlertView {
                id: 7,
                kind: "UnknownOrder".into(),
                detail: "venue order X".into(),
                proposed_event_count: 2,
            }],
            last_pass_ts: 100,
        };
        let empty = CoreSnapshot::empty("binance", "BTCUSDT");

        // Unfiltered: fires on the rising edge, then latches until the alert clears.
        let any = AlertRule::new("ra", RuleTrigger::ReconAlert { divergence_kind: None });
        let mut st = RuleState::default();
        assert!(eval_snapshot_rule(&any, &mut st, &empty, 1).is_none(), "no alert → no fire");
        assert!(eval_snapshot_rule(&any, &mut st, &with_alert, 2).is_some(), "rising edge fires");
        assert!(
            eval_snapshot_rule(&any, &mut st, &with_alert, 3).is_none(),
            "still present → latched"
        );
        assert!(eval_snapshot_rule(&any, &mut st, &empty, 4).is_none(), "clears → re-arm, no fire");
        assert!(eval_snapshot_rule(&any, &mut st, &with_alert, 5).is_some(), "re-appears → fires");

        // Kind filter: only the named DivergenceKind fires.
        let scoped = AlertRule::new(
            "rb",
            RuleTrigger::ReconAlert { divergence_kind: Some("PositionDrift".into()) },
        );
        assert!(
            eval_snapshot_rule(&scoped, &mut RuleState::default(), &with_alert, 6).is_none(),
            "kind mismatch must not fire"
        );
    }

    // ---- Feed / breaker / resolution signals ------------------------------------------------

    #[test]
    fn feed_signal_fires_on_the_requested_state_and_scope() {
        let degraded_rule = AlertRule::new(
            "fd",
            RuleTrigger::Feed { venue: Some("binance".into()), state: FeedState::Degraded },
        );
        // matching venue + degraded → fires.
        assert!(
            eval_signal_rule(
                &degraded_rule,
                &mut RuleState::default(),
                &AlertSignal::Feed { venue: "binance".into(), degraded: true },
                1
            )
            .is_some()
        );
        // a recovery signal must NOT fire a "degraded" rule.
        assert!(
            eval_signal_rule(
                &degraded_rule,
                &mut RuleState::default(),
                &AlertSignal::Feed { venue: "binance".into(), degraded: false },
                2
            )
            .is_none()
        );
        // wrong venue must not fire.
        assert!(
            eval_signal_rule(
                &degraded_rule,
                &mut RuleState::default(),
                &AlertSignal::Feed { venue: "okx".into(), degraded: true },
                3
            )
            .is_none()
        );
        // a "recovered" rule fires on the recovery signal.
        let recovered_rule =
            AlertRule::new("fr", RuleTrigger::Feed { venue: None, state: FeedState::Recovered });
        assert!(
            eval_signal_rule(
                &recovered_rule,
                &mut RuleState::default(),
                &AlertSignal::Feed { venue: "okx".into(), degraded: false },
                4
            )
            .is_some()
        );
    }

    #[test]
    fn breaker_and_resolution_signals_fire_on_their_own_kind_only() {
        let breaker = AlertRule::new(
            "b",
            RuleTrigger::FillRateBreaker { venue: None, symbol: Some("BTCUSDT".into()) },
        );
        assert!(
            eval_signal_rule(
                &breaker,
                &mut RuleState::default(),
                &AlertSignal::FillRateBreaker { venue: "binance".into(), symbol: "BTCUSDT".into() },
                1
            )
            .is_some()
        );
        // symbol mismatch → no fire.
        assert!(
            eval_signal_rule(
                &breaker,
                &mut RuleState::default(),
                &AlertSignal::FillRateBreaker { venue: "binance".into(), symbol: "ETHUSDT".into() },
                2
            )
            .is_none()
        );
        // a feed signal never fires a breaker rule.
        assert!(
            eval_signal_rule(
                &breaker,
                &mut RuleState::default(),
                &AlertSignal::Feed { venue: "binance".into(), degraded: true },
                3
            )
            .is_none()
        );

        let resolution = AlertRule::new(
            "pm",
            RuleTrigger::PolymarketResolution { token_id: Some("0xtok".into()) },
        );
        assert!(
            eval_signal_rule(
                &resolution,
                &mut RuleState::default(),
                &AlertSignal::PolymarketResolution { token_id: "0xtok".into() },
                4
            )
            .is_some()
        );
        assert!(
            eval_signal_rule(
                &resolution,
                &mut RuleState::default(),
                &AlertSignal::PolymarketResolution { token_id: "0xOTHER".into() },
                5
            )
            .is_none()
        );
    }

    // ---- cooldown / once gating -------------------------------------------------------------

    #[test]
    fn cooldown_suppresses_refire_within_the_window() {
        let mut rule = AlertRule::new("f", RuleTrigger::Fill { venue: None, symbol: None });
        rule.cooldown_ms = 1000;
        let mut st = RuleState::default();
        let f = fill_event("binance", "BTCUSDT");
        assert!(eval_event_rule(&rule, &mut st, &f, 1000).is_some(), "first fill fires");
        assert!(
            eval_event_rule(&rule, &mut st, &f, 1500).is_none(),
            "within cooldown → suppressed"
        );
        assert!(eval_event_rule(&rule, &mut st, &f, 2000).is_some(), "cooldown elapsed → fires");
    }

    #[test]
    fn once_fires_at_most_a_single_time() {
        let mut rule = AlertRule::new("f", RuleTrigger::Fill { venue: None, symbol: None });
        rule.once = true;
        let mut st = RuleState::default();
        let f = fill_event("binance", "BTCUSDT");
        assert!(eval_event_rule(&rule, &mut st, &f, 1).is_some());
        assert!(eval_event_rule(&rule, &mut st, &f, 2).is_none(), "once ⇒ never again");
        assert!(eval_event_rule(&rule, &mut st, &f, 9_999).is_none());
    }
}
