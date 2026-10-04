//! The PURE alert evaluator: `(rule + per-rule state + one input) -> Option<FiredAlert>`. No I/O,
//! no clocks (the caller passes `now_ms`), no delivery (that is [`crate::delivery`]) — so every
//! trigger's fire/no-fire is unit-tested with plain values. [`crate::AlertEngine`] holds the
//! [`RuleState`] map + the sinks and folds these functions over the enabled rules OFF the hot fold.
//!
//! **Nothing here is feature-gated, and nothing here names a vike type.** [`eval_snapshot_rule`]
//! reads its facts through [`SnapshotFacts`] and [`eval_event_rule`] takes an [`AlertEvent`], both
//! owned by this module, so every entry point compiles in the one vike-free build the standalone
//! recorder watchdog links. ⚠ Until 2026-09-23 those two took `vike_core::CoreSnapshot` and
//! `vike_model::events::Event` behind a `core` feature, and this paragraph described that split
//! until 2026-09-28. Its output type, [`FiredAlert`], lives in [`crate::delivery`] alongside the
//! sinks that consume it.
//!
//! Four inputs, one per evaluation entry point (mirrors [`RuleTrigger`]'s grouping):
//! - [`eval_snapshot_rule`] over the caller's [`SnapshotFacts`] — price crossings, drawdown, recon;
//! - [`eval_indicator_rule`] over an [`IndicatorSample`] the consumer computed;
//! - [`eval_event_rule`] over an [`AlertEvent`] — fills, rejects;
//! - [`eval_signal_rule`] over an [`AlertSignal`] — feed health, breaker trip, resolution, and a
//!   RECORDED SERIES that stopped receiving rows.
//!
//! Each function no-ops (returns `None`) for a rule whose trigger belongs to a different input, so
//! the engine can hand every enabled rule to the matching entry point without pre-partitioning.
//!
//! Edge/latch semantics live HERE (in [`RuleState`]): scalar crossings ([`Compare`]) edge-detect
//! via `last_value`; drawdown/recon LATCH via `latched`; discrete events/signals fire on each
//! occurrence, rate-limited only by the rule's cooldown/once (both applied by [`maybe_fire`]).

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
    /// subscription DECLARES (`vike_data::store::series_cadence`), and that declaration is the single gate
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

/// One reconcile alert as a RULE needs to see it — the three fields
/// [`RuleTrigger::ReconAlert`] matches on and renders, and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconAlertFact<'a> {
    /// Matched against [`RuleTrigger::ReconAlert::divergence_kind`] when that is `Some`.
    pub kind: &'a str,
    pub detail: &'a str,
    pub proposed_event_count: usize,
}

/// Everything a SNAPSHOT-driven rule reads, asked of the caller.
///
/// ⚠ **This exists so this crate names NO vike type at all.** It used to take a
/// `vike_core::CoreSnapshot`, which put a `vike-core` edge on a crate whose whole reason for being
/// split out of `vike-ops` was *"so a watchdog could page without it"* — the split was half-done,
/// and the rank paid for the other half (`docs/decisions/0085-the-rank-follows-the-declaration-not-the-role.md`).
/// The engine compares numbers; what produced them is not its business, and the caller already
/// holds the snapshot.
///
/// ⚠ **A TRAIT rather than a struct of borrowed slices**, and the reason is the test suite rather
/// than taste. A struct carrying `&[ReconAlertFact<'_>]` makes every builder hold three things at
/// once — the strings, a vector of facts pointing into them, and the struct pointing into that —
/// which a helper cannot return, because that is a self-referential value. The production caller
/// never notices (it borrows from a snapshot it already owns) but every test helper would have to
/// be inlined at its call site. A trait moves the ownership to the implementor, so a test writes
/// one plain struct and a helper returns it by value.
///
/// ⚠ **`mark` is a lookup, not a resolved price**, because it is keyed on the RULE's own
/// venue/symbol, which the caller cannot know in advance. It is called at most once per `Price`
/// rule and not at all for the others.
pub trait SnapshotFacts {
    /// Latest published mark for `(venue, symbol)`, or `None` where nothing has been published.
    fn mark(&self, venue: &str, symbol: &str) -> Option<f64>;

    /// The caller's OWN equity curve. ⚠ See the `Drawdown` arm of [`eval_snapshot_rule`] for why
    /// this must be that quantity and not a cross-venue equity total — the distinction is what an
    /// incident on 2026-08-17 turned on, and this crate can no longer check which one arrived.
    fn drawdown_curve(&self) -> f64;

    fn capital_base(&self) -> f64;
    fn pnl_total(&self) -> f64;

    /// The reconcile alerts outstanding right now. Returns an owned `Vec` because an implementor
    /// may have to shape them — it is built at most once per `ReconAlert` rule, off the hot fold.
    fn recon_alerts(&self) -> Vec<ReconAlertFact<'_>>;
}

/// One EVENT a rule can fire on — the event-shaped twin of [`SnapshotFacts`], carrying only the
/// fields [`eval_event_rule`] matches on or renders.
///
/// ⚠ Three variants, not the whole of `vike_model::events::Event`: those are the only ones any
/// rule has ever matched, and widening this enum is the honest way to add a fourth — as opposed to
/// naming the event crate and inheriting every variant it will ever grow.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AlertEvent<'a> {
    Fill { venue: &'a str, symbol: &'a str, side: i32, last_qty: f64, last_px: f64 },
    OrderRejected { client_order_id: &'a str, reason: &'a str },
    OrderDenied { client_order_id: &'a str, reason: &'a str },
}

/// Evaluate a snapshot-driven rule ([`RuleTrigger::Price`] / [`Drawdown`](RuleTrigger::Drawdown) /
/// [`ReconAlert`](RuleTrigger::ReconAlert)) against `facts`. Returns `None` for any other trigger.
pub fn eval_snapshot_rule(
    rule: &AlertRule,
    st: &mut RuleState,
    facts: &dyn SnapshotFacts,
    now_ms: i64,
) -> Option<FiredAlert> {
    match &rule.trigger {
        RuleTrigger::Price { venue, symbol, op, level } => {
            // No published mark yet ⇒ nothing to compare (and DON'T seed last_value from a phantom
            // — the first real mark then can't spuriously "cross").
            let cur = facts.mark(venue, symbol)?;
            if scalar_crossed(*op, st, cur, *level) {
                maybe_fire(rule, st, now_ms, format!("{venue} {symbol} {op} {level} (mark {cur})"))
            } else {
                None
            }
        }
        RuleTrigger::Drawdown { pct } => {
            // ⚠ The caller must fill `drawdown_curve` from `Portfolio::drawdown_curve`, NOT from
            // `CoreSnapshot::equity()`. This rule FOLLOWS the core's own latch onto the daemon's own
            // equity curve (configured capital + realized + unrealized P&L) instead of the
            // cross-venue equity TOTAL, which on an `Authoritative` block includes the venue's
            // wallet for the whole account the credentials open. Reading the total meant a third
            // party's withdrawal from a SHARED account fired this alert with no trading behind it,
            // while a real 25% loss on the daemon's own ~9000 of book was 3.6% of a 62647 total and
            // never fired at all (the CI box, 2026-08-17). The latch and the alert now measure the same
            // quantity by construction — `Portfolio::pnl_total`'s fold is pinned bit-identical to
            // the engine-side scalar the latch uses, so an operator can never be told "no drawdown"
            // about a core that just latched itself liquidate-only.
            //
            // ⚠ That pairing is now the CALLER's to keep: this crate no longer names `Portfolio`,
            // so nothing here can check which quantity arrived. The one production caller states it
            // at the conversion (`crates/vike-tradehub/src/alerts.rs`).
            let cur = facts.drawdown_curve();
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
                        facts.capital_base(),
                        facts.pnl_total()
                    ),
                )
            } else {
                None
            }
        }
        RuleTrigger::ReconAlert { divergence_kind: kind } => {
            let alerts = facts.recon_alerts();
            let matching = alerts.iter().find(|a| match kind {
                None => true,
                Some(k) => a.kind == k.as_str(),
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
/// against one [`AlertEvent`]. Returns `None` for any other trigger or a non-matching event.
///
/// ⚠ Takes [`AlertEvent`], NOT `vike_model::events::Event` — see [`SnapshotFacts`] for the whole
/// argument. The rendered text is unchanged: the same five fields in the same order, so an
/// operator's alert reads identically to before the inversion.
pub fn eval_event_rule(
    rule: &AlertRule,
    st: &mut RuleState,
    ev: &AlertEvent<'_>,
    now_ms: i64,
) -> Option<FiredAlert> {
    match (&rule.trigger, ev) {
        (
            RuleTrigger::Fill { venue, symbol },
            AlertEvent::Fill { venue: v, symbol: s, side, last_qty, last_px },
        ) => {
            if filter_matches(venue, v) && filter_matches(symbol, s) {
                maybe_fire(
                    rule,
                    st,
                    now_ms,
                    format!("fill {v} {s} side {side} qty {last_qty} @ {last_px}"),
                )
            } else {
                None
            }
        }
        (RuleTrigger::OrderRejected, AlertEvent::OrderRejected { client_order_id, reason }) => {
            maybe_fire(rule, st, now_ms, format!("order {client_order_id} rejected: {reason}"))
        }
        (RuleTrigger::OrderRejected, AlertEvent::OrderDenied { client_order_id, reason }) => {
            maybe_fire(rule, st, now_ms, format!("order {client_order_id} denied: {reason}"))
        }
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

/// The recorder-liveness tests — `SeriesStale`, `SeriesSlow` and `FamilyCollapse`, the signal
/// rules a recorder feeds. ⚠ This introduced them as "the vike-free half of this module's tests",
/// run in both builds and the only evaluator tests a standalone lane ran, until 2026-09-28: every
/// test in this module has been vike-free since `core` went on 2026-09-23, there is one build, and
/// that lane was deleted on 2026-09-28.
#[path = "stale_tests.rs"]
#[cfg(test)]
mod stale_tests;

#[path = "eval_tests.rs"]
#[cfg(test)]
mod eval_tests;
