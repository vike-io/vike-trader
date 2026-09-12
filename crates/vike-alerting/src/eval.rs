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
    /// A recorded SERIES is receiving rows and receiving far too few — see
    /// [`RuleTrigger::SeriesSlow`].
    ///
    /// ⚠ A separate variant rather than a second reading of [`SeriesStale`](Self::SeriesStale),
    /// and the reason is the BODY. `SeriesStale`'s two shapes render "stopped receiving rows" and
    /// "has NEVER received a row"; both are FALSE here — this series is receiving rows the whole
    /// time, which is exactly why it stayed invisible for forty days — and the diagnosis is the
    /// pair of numbers, observed against expected. A trigger that cannot carry them reports the
    /// fault as its opposite. (`vike_recorder::alerts`'s feed rule made the other trade, reusing
    /// `Feed` because its body carried nothing worth keeping; that argument does not hold here.)
    SeriesSlow {
        series: String,
        observed_per_s: f64,
        expected_per_s: f64,
        window_secs: i64,
        /// The sibling series whose activity licensed the verdict, and its rate — "the tape was
        /// busy and the book was not" is what tells an operator this is a broken lane rather than
        /// a dead market.
        governor: String,
        governor_per_s: f64,
    },
    /// A recorded FAMILY has stopped producing, judged against its OWN recent history — see
    /// [`RuleTrigger::FamilyCollapse`].
    ///
    /// ⚠ **A THIRD variant rather than a second reading of [`SeriesSlow`](Self::SeriesSlow), and
    /// the reason is one field.** `SeriesSlow::expected_per_s` MEANS a rate the venue's own
    /// subscription DECLARES (`vike_data::series_cadence`), and that declaration is the single gate
    /// between the cadence rule and an invented threshold. The number here is LEARNED — a rolling
    /// median of this family's own recent windows — so putting it in that field would smuggle an
    /// invented number into the exact place a declared one is promised. Hence no per-second field
    /// at all on this variant: only an item COUNT over a stated window, which cannot be mistaken
    /// for a cadence.
    ///
    /// `licence`/`licence_items` are the out-of-family witness that permitted the verdict — "this
    /// family produced nothing while THAT one produced N in the same window". Without it the body
    /// cannot separate a dead family from a stalled process, which is the whole diagnosis, and the
    /// producer refuses to take a verdict without one.
    FamilyCollapse {
        /// `{kind}/{venue}/{family}` — the subject with continuous existence across a rotation.
        family: String,
        observed_items: u64,
        /// The rolling median of this family's own recent completed windows.
        baseline_items: u64,
        window_secs: i64,
        /// How many member series keys the family held when the window closed.
        members: usize,
        /// How many completed windows the baseline was taken over.
        ring_windows: usize,
        licence: String,
        licence_items: u64,
    },
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

/// [`filter_matches`]' PREFIX twin, shared by the three recorder triggers
/// ([`RuleTrigger::SeriesStale`], [`SeriesSlow`](RuleTrigger::SeriesSlow),
/// [`FamilyCollapse`](RuleTrigger::FamilyCollapse)) — the first one's doc carries the reason an
/// exact series name cannot be the scope on a venue whose symbols rotate.
///
/// ⚠ It said "for `SeriesStale` alone" and had two more callers by then. A prefix on a FAMILY key
/// behaves identically because the first two segments of `{kind}/{venue}/{family}` are the first
/// two of every member's `{kind}/{venue}/{symbol}`, which is why one function serves all three.
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
/// [`SeriesStale`](RuleTrigger::SeriesStale) / [`SeriesSlow`](RuleTrigger::SeriesSlow) /
/// [`FamilyCollapse`](RuleTrigger::FamilyCollapse)) against one [`AlertSignal`].
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
        (
            RuleTrigger::SeriesSlow { series_prefix },
            AlertSignal::SeriesSlow {
                series,
                observed_per_s,
                expected_per_s,
                window_secs,
                governor,
                governor_per_s,
            },
        ) => {
            if !prefix_matches(series_prefix, series.as_str()) {
                return None;
            }
            // Both rates to two decimals: the fault is an ORDER-OF-MAGNITUDE shortfall (0.42
            // against 10), so precision past that is noise, and rounding to integers would render
            // the measured broken lane as "0".
            let body = format!(
                "series {series} is running at {observed_per_s:.2}/s against an expected \
                 {expected_per_s:.2}/s over {window_secs}s — while {governor} ran at \
                 {governor_per_s:.2}/s, so the instrument was busy and this lane was not"
            );
            maybe_fire(rule, st, now_ms, body)
        }
        (
            RuleTrigger::FamilyCollapse { series_prefix },
            AlertSignal::FamilyCollapse {
                family,
                observed_items,
                baseline_items,
                window_secs,
                members,
                ring_windows,
                licence,
                licence_items,
            },
        ) => {
            if !prefix_matches(series_prefix, family.as_str()) {
                return None;
            }
            // COUNTS, never a rate, and the word "baseline" never "expected": the second number is
            // this family's own recent median and the body must not read as an authority it is not.
            // The member count is in the line because it is what tells an operator this is the
            // WHOLE family and not one rotated-out token, and the licence is what tells them the
            // process was still receiving data.
            //
            // ⚠ The empty-licence arm is not decoration. A recorder that records exactly ONE
            // family has nothing outside it to ask, and its producer waives the licence rather than
            // being permanently unable to fire; rendering that case through the sentence below
            // would assert corroboration that does not exist, which is precisely the false
            // diagnosis this whole rule is careful about.
            let witness = if licence.is_empty() {
                " — and this process records no other series, so nothing corroborates it"
                    .to_string()
            } else {
                format!(
                    ", while {licence} produced {licence_items} in the SAME window, so this \
                     recorder was still receiving data and this family was not"
                )
            };
            let body = format!(
                "family {family} produced {observed_items} items in {window_secs}s across \
                 {members} member series — against a rolling baseline of {baseline_items} \
                 (median of its own last {ring_windows} windows){witness}"
            );
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

    // ---- the FAMILY-collapse rule -------------------------------------------------------------

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

    #[test]
    fn a_family_collapse_names_the_family_both_counts_and_the_licence() {
        let rule = AlertRule::new("f", RuleTrigger::FamilyCollapse { series_prefix: None });
        let fired = eval_signal_rule(
            &rule,
            &mut RuleState::default(),
            &collapse("book/polymarket/btc-updown-5m"),
            1,
        )
        .expect("an unscoped rule fires for any family");
        let b = &fired.body;
        assert!(b.contains("book/polymarket/btc-updown-5m"), "the family is named: {b}");
        assert!(b.contains("53485"), "the baseline it fell from belongs in the body: {b}");
        assert!(b.contains('0'), "…and the observed count: {b}");
        assert!(b.contains("4 member"), "the member count separates a family from one token: {b}");
        assert!(
            b.contains("trade/binance/BTCUSDT.P") && b.contains("435"),
            "the licence is the diagnosis — without it a dead family and a stalled process read \
             the same: {b}"
        );
    }

    /// ⚠ **The body must never state a per-second rate.** The second number is LEARNED, and
    /// `SeriesSlow`'s is DECLARED; rendering this one as "/s" would present an invented expectation
    /// in the vocabulary reserved for a real one, which is the whole reason this variant exists
    /// rather than a second reading of that one.
    #[test]
    fn the_family_body_states_counts_and_never_a_cadence() {
        let rule = AlertRule::new("f", RuleTrigger::FamilyCollapse { series_prefix: None });
        let fired =
            eval_signal_rule(&rule, &mut RuleState::default(), &collapse("book/polymarket/f"), 1)
                .unwrap();
        assert!(!fired.body.contains("/s"), "no per-second figure may appear: {}", fired.body);
        assert!(
            !fired.body.contains("expected"),
            "and not the word that means a DECLARED cadence: {}",
            fired.body
        );
    }

    /// **A verdict with NO out-of-family witness says so, rather than asserting one.**
    ///
    /// A recorder that records exactly one family has nothing outside it to ask, so its producer
    /// waives the licence rather than being permanently unable to fire. Rendering that through the
    /// ordinary sentence would claim "another series kept writing" when none exists — a false
    /// diagnosis, in the one field an operator uses to tell a dead family from a stalled process.
    #[test]
    fn a_verdict_with_no_witness_never_claims_one() {
        let rule = AlertRule::new("f", RuleTrigger::FamilyCollapse { series_prefix: None });
        let sig = AlertSignal::FamilyCollapse {
            family: "book/polymarket/btc-updown-5m".to_string(),
            observed_items: 0,
            baseline_items: 22_285,
            window_secs: 30,
            members: 4,
            ring_windows: 20,
            licence: String::new(),
            licence_items: 0,
        };
        let fired = eval_signal_rule(&rule, &mut RuleState::default(), &sig, 1).unwrap();
        assert!(
            fired.body.contains("no other series"),
            "an unwitnessed verdict must say it is unwitnessed: {}",
            fired.body
        );
        assert!(
            !fired.body.contains("still receiving data"),
            "…and must not claim corroboration it does not have: {}",
            fired.body
        );
    }

    /// The prefix scope reaches a FAMILY key, whose first two segments are a member series key's
    /// first two — so one profile setting scopes all three recorder rules the same way.
    #[test]
    fn the_family_scope_is_a_prefix_over_the_family_key() {
        let rule = AlertRule::new(
            "f",
            RuleTrigger::FamilyCollapse { series_prefix: Some("book/polymarket/".into()) },
        );
        assert!(
            eval_signal_rule(
                &rule,
                &mut RuleState::default(),
                &collapse("book/polymarket/btc-updown-5m"),
                1
            )
            .is_some()
        );
        assert!(
            eval_signal_rule(
                &rule,
                &mut RuleState::default(),
                &collapse("trade/binance/BTCUSDT.P"),
                2
            )
            .is_none(),
            "a scoped rule must not fire for a family outside its scope"
        );
    }

    /// Cross-talk in both directions, as the two rules beside it already pin: a family signal must
    /// not fire a series rule and a series signal must not fire the family rule. Three rule ids,
    /// three faults — an operator correlating on one must not receive another.
    #[test]
    fn a_family_signal_and_rule_do_not_cross_with_the_series_kinds() {
        let fam_rule = AlertRule::new("f", RuleTrigger::FamilyCollapse { series_prefix: None });
        assert!(
            eval_signal_rule(
                &fam_rule,
                &mut RuleState::default(),
                &stale("book/polymarket/0xtok", Some(1), 1),
                1
            )
            .is_none()
        );
        for other in [
            RuleTrigger::SeriesStale { series_prefix: None },
            RuleTrigger::SeriesSlow { series_prefix: None },
        ] {
            let rule = AlertRule::new("o", other);
            assert!(
                eval_signal_rule(
                    &rule,
                    &mut RuleState::default(),
                    &collapse("book/polymarket/btc-updown-5m"),
                    2
                )
                .is_none(),
                "a family signal must not fire {:?}",
                rule.trigger
            );
        }
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
