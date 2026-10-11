//! The inputs the entry points read: signals, indicator samples, snapshot facts, events.

#[cfg(doc)]
use super::{eval_event_rule, eval_indicator_rule, eval_snapshot_rule};
#[cfg(doc)]
use crate::rule::RuleTrigger;

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
/// ⚠ **This exists so this crate names NO vike type at all**
/// (`docs/decisions/0085-the-rank-follows-the-declaration-not-the-role.md`). The engine compares
/// numbers; what produced them is not its business, and the caller already holds the snapshot.
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
