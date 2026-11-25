//! The inputs the entry points read: signals, indicator samples, snapshot facts, events.

#[cfg(doc)]
use super::{evaluation_event_rule, evaluation_indicator_rule, evaluation_snapshot_rule};
use crate::rule::Compare;
#[cfg(doc)]
use crate::rule::RuleTrigger;

/// A transient status the off-fold consumer maps producer state onto (never persisted — signals are
/// live edges, not config). The consumer emits one only on a TRANSITION (e.g. a `StreamHealth`
/// `StreamStatus`, a `SpreadMaker` breaker trip, a Polymarket resolution), so signal rules fire on
/// each occurrence with no latch of their own.
#[derive(Debug, Clone, PartialEq)]
pub enum AlertSignal {
    /// A feed changed health. `degraded == true` for a `StreamStatus::GapStart`/`Stale`, `false` for
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
    /// One measurement for ONE [`RuleTrigger::ModelDrift`] rule — see that trigger. Boxed: the reading is the
    /// widest signal by far and is sent once per measuring pass.
    ModelDrift(Box<ModelDriftReading>),
}

/// What the caller measured for a [`RuleTrigger::ModelDrift`] rule: the live forecasts of its window against
/// its reference run's. Plain numbers, so this crate names no statistic's code; a NaN statistic (an empty
/// sample) is never a drift.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelDriftReading {
    /// The id of the rule this was measured for (`AlertRule::id`): the rule's window, mount and reference
    /// decide the numbers, so a reading belongs to exactly one rule.
    pub rule_id: String,
    /// How many live forecasts the window held.
    pub forecasts: usize,
    /// How many forecasts the reference run held for the symbol.
    pub reference_forecasts: usize,
    /// The population stability index of the live values against the reference's.
    pub population_stability_index: f64,
    /// The two-sample Kolmogorov–Smirnov statistic `D` and its p-value.
    pub kolmogorov_smirnov_statistic: f64,
    pub kolmogorov_smirnov_p_value: f64,
    /// The realised-error check, when the rule asks for one and it could be measured.
    pub error: Option<ForecastErrorReading>,
}

/// The realised-error half of a [`ModelDriftReading`].
#[derive(Debug, Clone, PartialEq)]
pub struct ForecastErrorReading {
    /// How many live forecasts had a realised return to be measured against.
    pub errors: usize,
    /// Their mean absolute error.
    pub mean: f64,
    /// The reference run's mean absolute error, the CUSUM's target.
    pub reference_mean: f64,
    /// How many times the CUSUM alarmed over the live errors.
    pub alarms: usize,
}

/// One streaming-indicator reading the off-fold consumer computed (over `vike_indicators` /
/// `vike_chart::chart::study`) and feeds to [`evaluation_indicator_rule`]. Keeping the value on the input
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
/// [`RuleTrigger::ReconciliationAlert`] matches on and renders, and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconcileAlertFact<'a> {
    /// Matched against [`RuleTrigger::ReconciliationAlert::divergence_kind`] when that is `Some`.
    pub kind: &'a str,
    pub detail: &'a str,
    pub proposed_event_count: usize,
}

/// One NON-primary currency group's drawdown curve, as the caller's core measured it
/// (`vike_execution::Portfolio::group_curves`). Plain owned data, like every fact here.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupCurveFact {
    /// The group's currency code for a message (`"EUR"`; the dollar group reads `"USD"`).
    pub currency: String,
    /// The group's curve (`capital base + own PnL`) at the core's last sweep, in the group's currency.
    pub curve: f64,
    /// The group's high-water mark of that curve, the core's own.
    pub peak: f64,
}

/// One conversion rate that is STALE while a position is open in a symbol quoted in its currency — what
/// [`RuleTrigger::StaleRate`] fires on. Plain owned data like every fact here; the caller derives it from
/// the engine's own rate table (`vike_execution::VenueBlock::quote_rates`, a row that is `stale` and `held`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleRateFact {
    /// The account holding the position (its route key: the bare venue, or `venue#LABEL`).
    pub account: String,
    /// The quote currency whose rate is stale (`"JPY"`).
    pub currency: String,
    /// The currency the account books in (`"USD"`), the rate being account-currency units per `currency`.
    pub account_ccy: String,
}

/// Everything a SNAPSHOT-driven rule reads, asked of the caller.
///
/// ⚠ **This exists so this crate names NO vike type at all**
/// (`docs/decisions/0085-the-rank-follows-the-declaration-not-the-role.md`). The engine compares
/// numbers; what produced them is not its business, and the caller already holds the snapshot.
///
/// ⚠ **A TRAIT rather than a struct of borrowed slices**, and the reason is the test suite rather
/// than taste. A struct carrying `&[ReconcileAlertFact<'_>]` makes every builder hold three things at
/// once — the strings, a vector of facts pointing into them, and the struct pointing into that —
/// which a helper cannot return, because that is a self-referential value. The production caller
/// never notices (it borrows from a snapshot it already owns) but every test helper would have to
/// be inlined at its call site. A trait moves the ownership to the implementor, so a test writes
/// one plain struct and a helper returns it by value.
///
/// ⚠ **`last_price` is a lookup, not a resolved price**, because it is keyed on the RULE's own
/// venue/symbol, which the caller cannot know in advance. It is called at most once per `Price`
/// rule and not at all for the others.
pub trait SnapshotFacts {
    /// The LAST TRADED price of `(venue, symbol)`, the line a chart draws (owner, 2026-11-15: an alert
    /// compares against it, never a mid), or `None` where the caller holds none. `op` is the crossing the
    /// rule watches: a caller with no trade but a quote answers the quote's SIDE for it (the ask for
    /// [`Compare::Above`], the bid for [`Compare::Below`], as a buy stop and a sell stop trigger).
    fn last_price(&self, venue: &str, symbol: &str, op: Compare) -> Option<f64>;

    /// The caller's OWN equity curve. ⚠ See the `Drawdown` arm of [`evaluation_snapshot_rule`] for why
    /// this must be that quantity and not a cross-venue equity total — the distinction is what an
    /// incident on 2026-08-17 turned on, and this crate can no longer check which one arrived.
    fn drawdown_curve(&self) -> f64;

    /// The caller's own high-water mark of [`Self::drawdown_curve`], when it keeps one: the rule then measures
    /// the drop from it, so the alert and the latch agree after a restart under water (the latch seeds its peak
    /// at the capital base, a first reading would seed it at the drowned curve). `None`, the default (no latch
    /// armed, or before its first sweep), leaves the rule on its own session peak.
    fn drawdown_peak(&self) -> Option<f64> {
        None
    }

    fn capital_base(&self) -> f64;
    fn pnl_total(&self) -> f64;

    /// The drawdown curve and high-water mark of every currency group but the one
    /// [`Self::drawdown_curve`] is in (decision 0183, item 13): empty for a caller whose engines share one
    /// currency, which is every caller today. Owned, built at most once per `Drawdown` rule.
    fn group_curves(&self) -> Vec<GroupCurveFact> {
        Vec::new()
    }

    /// The currency [`Self::drawdown_curve`] is in (`"USD"` for the dollar group, `"EUR"`): the PRIMARY currency
    /// group's, the one the rule keeps its session peak in. A rule whose caller says another one than it last
    /// saw starts that peak over (a peak in euros is no peak of a curve in dollars), so the first live engine
    /// restarting into another currency mid-session no longer leaves the old peak standing until the daemon
    /// restarts. The default is the empty string: a caller with one currency never changes it, and the rule
    /// behaves as it always did.
    fn drawdown_currency(&self) -> String {
        String::new()
    }

    /// Every conversion rate that is stale while a position is open in its currency, right now (decision 0183).
    /// Empty for a caller whose accounts hold no symbol quoted in another currency, which is every account on a
    /// venue quoted in its own. Owned, built at most once per [`RuleTrigger::StaleRate`] rule.
    fn stale_rates(&self) -> Vec<StaleRateFact> {
        Vec::new()
    }

    /// The reconcile alerts outstanding right now. Returns an owned `Vec` because an implementor
    /// may have to shape them — it is built at most once per `ReconciliationAlert` rule, off the hot
    /// fold.
    fn reconcile_alerts(&self) -> Vec<ReconcileAlertFact<'_>>;
}

/// One EVENT a rule can fire on — the event-shaped twin of [`SnapshotFacts`], carrying only the
/// fields [`evaluation_event_rule`] matches on or renders.
///
/// ⚠ A few variants, not the whole of `vike_model::events::Event`: those are the only ones any
/// rule has ever matched, and widening this enum is the honest way to add another — as opposed to
/// naming the event crate and inheriting every variant it will ever grow. [`Self::JournalGrowth`] and
/// [`Self::JournalFenceHalt`] are no `Event` at all: the caller's own journal watch and core state them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AlertEvent<'a> {
    Fill {
        venue: &'a str,
        symbol: &'a str,
        side: i32,
        last_qty: f64,
        last_price: f64,
    },
    /// `refusal_id` is the refusal's catalog id (`vike_model::events::OrderRejected::refusal_id`), shown beside the
    /// reason as [`Self::OrderDenied`]'s, never decided on.
    OrderRejected {
        order_client_id: &'a str,
        venue: &'a str,
        refusal_id: Option<&'a str>,
        reason: &'a str,
    },
    /// `refusal_id` is the denial's catalog id (`vike_model::events::OrderDenied::refusal_id`), shown beside the
    /// reason, never decided on.
    OrderDenied {
        order_client_id: &'a str,
        venue: &'a str,
        refusal_id: Option<&'a str>,
        reason: &'a str,
    },
    /// The caller's write-ahead journal went over its size or growth alert ([`RuleTrigger::JournalGrowth`]): sent
    /// ONCE per crossing by the caller, which measures and keeps the latch. Its bytes on disk, its segment count,
    /// and how fast it grew over the caller's window (`None` while the window is too short to judge).
    JournalGrowth {
        total_bytes: u64,
        segments: u64,
        bytes_per_hour: Option<u64>,
    },
    /// The caller's node HALTED because its journal could not write ([`RuleTrigger::JournalFenceHalt`]): sent ONCE
    /// per halt by the caller, which decides on the snapshot's typed fact. `fault` is the node's own words, the body.
    JournalFenceHalt {
        fault: &'a str,
    },
}
