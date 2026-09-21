//! `BacktestReport` — the printable/serializable metrics summary: composes the EXISTING
//! `crate::metrics` functions over a [`BacktestResult`] into one flat, `Serialize` struct.
//!
//! This module does no metric MATH of its own — every number is a direct pass-through or a
//! single `metrics::` call, so a divergence here is a wiring bug, not a math bug (see the
//! `matches_direct_metrics_calls` test, which asserts field-for-field equality against calling
//! `metrics::` directly on the same inputs).
//!
//! FEATURE-FREE by design (it is `crate::report`, not `vike_backtest::harness::report`). The
//! composition needs nothing but `serde` + `crate::metrics`, which is exactly why it lives in this
//! vike-model-only crate — while `harness` as a whole sits behind vike-backtest's `hist-replay`
//! feature (which pulls the optional vike-data + rayon deps; the concrete DataFusion backend is a
//! further opt-in, `datafusion-store`). Living here lets the DataFusion-free leaf crates build the
//! SAME summary instead of re-assembling their own: `vike-report`'s `LiveTearsheet` composes this
//! struct for the fields they share, so a live tearsheet and a backtest tearsheet cannot drift
//! apart. `vike_backtest::harness::report` re-exports everything below (so the `backtest` bin's
//! paths are unchanged) and adds only the pieces that genuinely need the gated profile parser:
//! `periods_per_year(&BacktestProfile)` and `realism_stamp(&BacktestProfile)`.
//!
//! ⚠ **That "fields they share" used to be EIGHT of twenty-six, and the gap was the defect.** The
//! live tearsheet composed Sortino, Calmar, CAGR, SQN, VaR and expected shortfall with its own
//! `metrics::` calls and its own `0.95`, while a backtest report carried none of them — so the
//! shared subset could not drift and everything outside it was free to. [`ExtendedMetrics`] is the
//! one composition of those thirty numbers and both doors take them from it; the shared subset is
//! now the whole of both.

use serde::{Deserialize, Serialize};
use std::fmt;
// `writeln!` into a `String` (the `render_metrics` builder) needs the trait in scope; anonymous so
// it cannot be confused with `fmt::Display`'s formatter, which is what every other `writeln!` in
// this file targets.
use std::fmt::Write as _;

use crate::metrics;
use crate::result::BacktestResult;

/// Annualization factor for daily (`1d`) bars — the LEAN/tearsheet convention.
pub const DAILY_PERIODS_PER_YEAR: f64 = 252.0;
/// Fallback for non-daily / tick runs (a tick stream has no fixed period) — kept equal to
/// [`DAILY_PERIODS_PER_YEAR`] today so the Sharpe scale is at least consistent.
pub const DEFAULT_PERIODS_PER_YEAR: f64 = 252.0;

/// Milliseconds in one 24-hour day — the unit [`vike_model::time::interval_ms`] counts in.
const MS_PER_DAY: f64 = 86_400_000.0;

/// The annualization factor for a bar series of `interval`: the number of RETURN OBSERVATIONS a
/// year produces at that bar step, which is exactly what [`metrics::sharpe`]'s
/// `sqrt(periods_per_year)` needs.
///
/// # THE ONE HOME for this derivation, and why it moved here
///
/// It lives beside the constants it scales because TWO planes need it and only one ever had it.
/// `vike_backtest::harness::report::periods_per_year` is now a profile-shaped wrapper over this
/// function; `vike-studio-core`'s slice-shaped callers pass their `DataSlice::interval` straight
/// in. Before this function existed the Studio plane passed a bare `252.0`, so the SAME strategy
/// over the SAME 1h bars reported an `oos_sharpe` differing by `sqrt(24) ≈ 4.9x` between the
/// CLI/MCP door and the Studio door — while a doc comment on the harness side asserted the two
/// could not disagree, and no test compared them.
///
/// Keyed on the interval STRING rather than on a profile: a `BacktestProfile` is a
/// `hist-replay`-gated vike-backtest type the Studio plane has no business constructing, and the
/// interval is the only fact the derivation actually consumes.
///
/// # The scale
///
/// The daily anchor is PRESERVED exactly (`"1d"` returns [`DAILY_PERIODS_PER_YEAR`], so no
/// existing daily report moves by a single bit) and every other interval scales off it by how many
/// of that interval fit in a day: `252 · (86_400_000 / interval_ms)`. So `1h` -> 6,048 and
/// `1m` -> 362,880.
///
/// Two deliberate limits, stated rather than hidden:
///
/// * The 252 anchor is the LEAN/tearsheet EQUITY convention (252 trading days). These markets
///   trade 24/7, so a defensible crypto anchor is 365. Changing it would move every existing daily
///   report, which is a separate decision from fixing the intraday scale — so 252 stays and the
///   intraday values inherit it.
/// * An interval [`vike_model::time::interval_ms`] cannot parse (or a non-positive one) falls back
///   to [`DEFAULT_PERIODS_PER_YEAR`] rather than fabricating a scale — this is a reporting knob,
///   not a validation site, and each caller's own parser is what rejects a malformed interval.
///
/// A caller with no fixed period AT ALL — a tick stream — does not call this at all: it uses
/// [`DEFAULT_PERIODS_PER_YEAR`] directly, because there is no honest observation count to derive.
/// That branch stays with the caller because only the caller knows it is holding ticks.
pub fn periods_per_year_for_interval(interval: &str) -> f64 {
    match vike_model::time::interval_ms(interval) {
        Some(ms) if ms > 0 => DAILY_PERIODS_PER_YEAR * (MS_PER_DAY / ms as f64),
        _ => DEFAULT_PERIODS_PER_YEAR,
    }
}

/// A flat, `Serialize`-able summary of one backtest run. Every field is either copied straight
/// from the [`BacktestResult`] or computed by an existing `crate::metrics` function — see the
/// module doc.
///
/// The SCALARS are deliberately compact — they are the `backtest` bin's human table, the sweep's
/// ranking source and every reading verb's column set, and none of those wants an
/// everything-drawer.
///
/// ⚠ **The long-form catalog is no longer somebody else's problem, and this paragraph used to say
/// it was.** It read "a caller needing the long-form stat catalog (Sortino/Calmar/CAGR/SQN/VaR/…)
/// composes this for the shared fields and adds its own from `metrics::` — `vike_report::
/// LiveTearsheet` is the worked example", and what that arrangement produced was measurable: the
/// LIVE door printed twenty-six numbers over a [`BacktestResult`] and the BACKTEST door printed
/// eight over the same one, so the same fills answered a richer question depending on which verb
/// was asked. Those thirty numbers are [`Self::extended`] now, composed by one site
/// ([`ExtendedMetrics`]) that both doors take them from — and stored, because
/// `vike_model::runs::RunSeries` decimates the equity curve and a reader recomputing a VaR from
/// what is on disk would get a statistic of a thinned curve. [`crate::metric_catalog`] is the
/// roster over both halves.
///
/// # ⚠ SEVEN fields carry `#[serde(default)]`, and the other seven do not
///
/// This type is the content of `report.json` inside `<project>/user_data/runs/<run_id>/` — a
/// document written to people's disks since before it could be read back at all. Every field whose
/// own doc records it as added after the first version shipped ([`Self::profit_factor`],
/// [`Self::funding_paid`], [`Self::per_symbol_pnl`], [`Self::zero_trade`], [`Self::extended`],
/// [`Self::honesty`], [`Self::realism`]) MUST default, because a
/// required field makes every report written before it existed fail to parse — the failure
/// `vike_model::runs::RunManifest::schema` carries the same defence against, where a parse
/// failure becomes a DROPPED ROW in `crates/vike-studio-core/src/listing.rs`'s `list_runs`.
///
/// ⚠ For the three `Option` blocks that defaulting has a SECOND consequence, and it is the one to
/// carry: `None` means THIS RUN DID NOT RECORD IT, which is not the same answer as a zero. A reader
/// that renders `0.0` for an unrecorded Sortino has published "this strategy had no downside", and
/// `crate::metric_catalog::MetricSelection::needs_extended` exists so it can refuse instead.
///
/// ⚠ **The other seven are REQUIRED, deliberately, and defaulting them was a real defect.** They
/// have existed since the document's first version, so no `report.json` on anyone's disk can lack
/// one — defaulting buys nothing and removes the only structural check that the bytes are a report
/// AT ALL. With every field optional, `serde_json::from_str::<BacktestReport>("{}")` SUCCEEDS and
/// any unrelated JSON object reads as a real run of `final_equity 0.0 · sharpe 0.0 · n_trades 0`,
/// which the reading verbs this derive exists for would then render and compare against a
/// baseline. Before the derive that was a parse error, and it must stay one.
///
/// This is the same rule `RunManifest` states from the other side — "Every other field here is
/// REQUIRED" — and the two persisted documents of a run directory must not disagree about it.
///
/// ⚠ One caveat that is SERDE's rather than this type's: [`Self::name`] is a bare `Option`, and
/// serde's own `missing_field` helper answers `None` for an absent `Option` key whatever attributes
/// the field carries. So `name` is optional at read time and cannot be made otherwise here. The six
/// required SCALARS are what refuse a document that is not a report, and they are enough — `{}`
/// fails on the first of them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestReport {
    /// The profile's free-form `name`, if it had one (`BacktestProfile::name`).
    pub name: Option<String>,
    pub final_equity: f64,
    /// Fractional return from the first to the last equity-curve point (`metrics::total_return`).
    pub total_return: f64,
    pub n_trades: usize,
    /// Fraction of trades with positive PnL (`metrics::win_rate`).
    pub win_rate: f64,
    /// Annualized Sharpe of per-bar/per-tick returns (`metrics::sharpe`).
    pub sharpe: f64,
    /// Largest peak-to-trough drop as a positive fraction of the peak (`metrics::max_drawdown`).
    pub max_drawdown: f64,
    /// Gross profit / gross loss (`metrics::profit_factor`). House sentinel: `f64::INFINITY` when
    /// there are no losing trades but some profit, `0.0` when there is neither. Serialized as
    /// `null` when non-finite, so a `--json` consumer sees an explicit "no meaningful ratio"
    /// rather than a number — see [`ser_f64_null_when_nonfinite`]. Deliberately NOT printed by
    /// `Display` — the human table's row set predates this field and stays byte-identical; the
    /// field exists for ranking objectives (`vike_backtest::objective`) and JSON consumers.
    // ⚠ `default` is NOT redundant beside `deserialize_with`: serde calls the custom
    // deserializer only for a key that is PRESENT, so without this the field is REQUIRED and every
    // report.json predating it fails to parse. Absent -> `0.0`, which is what
    // `crate::metrics::profit_factor` answers when there is neither gross profit nor gross loss —
    // "no meaningful ratio", the same reading. A present `null` still routes through
    // `de_f64_null_as_infinity` and comes back as the `inf` sentinel.
    #[serde(
        default,
        serialize_with = "ser_f64_null_when_nonfinite",
        deserialize_with = "de_f64_null_as_infinity"
    )]
    pub profit_factor: f64,
    /// NET perp funding cashflow over the run (received-positive / paid-negative) — the twin of
    /// `vike_exec::Account.funding_paid`, straight from [`BacktestResult::funding_paid`]. `0.0` for a
    /// non-perp / no-funding run. Printed by `Display` ONLY when nonzero (so a spot backtest's table
    /// stays byte-identical to before this field existed); always present in `--json`.
    #[serde(default)]
    pub funding_paid: f64,
    /// Multi-symbol event runs only; empty for single-symbol/vector runs (see
    /// [`BacktestResult::per_symbol_pnl`]).
    #[serde(default)]
    pub per_symbol_pnl: Vec<(String, f64)>,
    /// Diagnosis of a zero-trade / flat-equity run — a RANKED list of probable causes composed by
    /// [`crate::zero_trade::ZeroTradeReport::analyze`] from the diagnostic counters the run already
    /// carried. `Some` ONLY when the run closed no trades AND its equity never moved (see
    /// `analyze`), so it is `None` — skipped on serialization (like the sibling `score` field) and
    /// absent from `Display` — for any run with trades, keeping a normal report byte-identical to
    /// before this field existed. When present, `Display` prints the diagnosis INSTEAD OF the
    /// bare all-zero metrics table.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zero_trade: Option<crate::zero_trade::ZeroTradeReport>,
    /// The LONG-FORM metric catalog — Sortino, Calmar, CAGR, SQN, VaR, expected shortfall, return
    /// skew and the rest of [`crate::metric_catalog::METRICS`]. See [`ExtendedMetrics`] for why it
    /// is computed at RUN time and stored rather than derived by a reader.
    ///
    /// `None` for a `report.json` written before this block existed, and for a report built by a
    /// literal rather than by [`BacktestReport::from_result`]. A reader must refuse a selection
    /// that needs it rather than rendering zeros — the distinction
    /// [`crate::metric_catalog::MetricSelection::needs_extended`] exists to make askable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extended: Option<ExtendedMetrics>,
    /// What the run DEFERRED, skipped, could not price and refused — see [`HonestyCounters`].
    ///
    /// `None` on the same terms as [`Self::extended`]. Printed by `Display` only when something is
    /// non-zero, so a clean run's table stays byte-identical to before this field existed (the same
    /// discipline [`Self::funding_paid`] follows).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub honesty: Option<HonestyCounters>,
    /// The cost model the run actually ran under — see [`crate::realism::RealismStamp`].
    ///
    /// ⚠ **`from_result` leaves this `None` and that is deliberate**: the stamp is resolved from a
    /// `BacktestProfile`, which is a `hist-replay`-gated type this crate has no business naming, so
    /// the producer attaches it with [`BacktestReport::with_realism`] instead. A `None` therefore
    /// means "nobody stamped this run", which is a different answer from a frictionless one and
    /// must not be rendered as one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub realism: Option<crate::realism::RealismStamp>,
}

/// The long-form metric catalog for one run: every [`crate::metric_catalog::METRICS`] id whose
/// `home` is [`crate::metric_catalog::MetricHome::Extended`], each one a single `crate::metrics`
/// call over the same [`BacktestResult`] the compact scalars were composed from.
///
/// # Why this is STORED rather than derived by a reader
///
/// A reader holding a run record could recompute most of these — and would get different numbers.
/// `vike_model::runs::RunSeries` DECIMATES the equity curve once a run exceeds
/// `vike_model::runs::MAX_EQUITY_SAMPLES`, so a Sortino or a VaR recomputed from what is on disk is
/// a statistic of a thinned curve, shallower and smoother than the one the run actually produced.
/// That is exactly the argument `crates/vike-cli/src/cmd/runs/show.rs` already makes when it
/// refuses `--breakdown`. Computing here, once, from the whole curve, is the only spelling in which
/// the stored number and the run's own Sharpe come from the same samples.
///
/// # Nothing here is new math
///
/// Every field is one `crate::metrics` call, exactly as [`BacktestReport`]'s own fields are — so a
/// divergence in this struct is a wiring bug, not a math bug, and
/// `extended_matches_direct_metrics_calls` asserts it field-for-field.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExtendedMetrics {
    // --- the trade ledger's own statistics ---
    pub net_profit: f64,
    pub gross_profit: f64,
    pub gross_loss: f64,
    pub total_fees: f64,
    pub avg_win: f64,
    pub avg_loss: f64,
    pub largest_win: f64,
    pub largest_loss: f64,
    pub payoff_ratio: f64,
    pub expected_payoff: f64,
    pub consecutive_wins: usize,
    pub consecutive_losses: usize,
    pub sqn: f64,
    pub long_ratio: f64,
    // --- the equity curve's own statistics ---
    pub sortino: f64,
    pub calmar: f64,
    pub cagr: f64,
    pub mar_ratio: f64,
    pub recovery_factor: f64,
    pub ulcer_index: f64,
    pub ulcer_performance_index: f64,
    pub k_ratio: f64,
    pub risk_return_ratio: f64,
    pub returns_volatility: f64,
    pub returns_skewness: f64,
    pub returns_kurtosis: f64,
    pub tail_ratio: f64,
    pub omega: f64,
    pub value_at_risk_95: f64,
    pub expected_shortfall_95: f64,
}

/// The threshold [`crate::metrics::omega`] is computed against: a return of zero, i.e. "gains over
/// losses". A non-zero threshold is a different question (excess over a target), and picking one
/// here would bake a target nobody chose into every stored report.
pub const OMEGA_THRESHOLD: f64 = 0.0;

/// The confidence the stored VaR and expected shortfall are computed at. 95% is the tearsheet
/// convention `vike_report::LiveTearsheet` already prints at, and the two must match or the live
/// and backtest doors answer the same question with different tails.
pub const TAIL_CONFIDENCE: f64 = 0.95;

impl ExtendedMetrics {
    /// Compose the long-form catalog from a raw [`BacktestResult`]. `periods_per_year` is the same
    /// annualization factor [`BacktestReport::from_result`] was given, so Sharpe and Sortino are on
    /// one scale — passing a different one here is the `sqrt(24)` class of bug
    /// [`periods_per_year_for_interval`]'s doc records.
    pub fn from_result(r: &BacktestResult, periods_per_year: f64) -> Self {
        let eq = &r.equity_curve;
        let tr = &r.trades;
        ExtendedMetrics {
            net_profit: metrics::net_profit(tr),
            gross_profit: metrics::gross_profit(tr),
            gross_loss: metrics::gross_loss(tr),
            total_fees: metrics::total_fees(tr),
            avg_win: metrics::avg_win(tr),
            avg_loss: metrics::avg_loss(tr),
            largest_win: metrics::largest_win(tr),
            largest_loss: metrics::largest_loss(tr),
            payoff_ratio: metrics::payoff_ratio(tr),
            expected_payoff: metrics::expected_payoff(tr),
            consecutive_wins: metrics::consecutive_wins(tr),
            consecutive_losses: metrics::consecutive_losses(tr),
            sqn: metrics::sqn(tr),
            long_ratio: metrics::long_ratio(tr),
            sortino: metrics::sortino(eq, periods_per_year),
            calmar: metrics::calmar(eq, periods_per_year),
            cagr: metrics::cagr(eq, periods_per_year),
            mar_ratio: metrics::mar_ratio(eq, periods_per_year),
            recovery_factor: metrics::recovery_factor(eq),
            ulcer_index: metrics::ulcer_index(eq),
            ulcer_performance_index: metrics::ulcer_performance_index(eq, periods_per_year),
            k_ratio: metrics::k_ratio(eq),
            risk_return_ratio: metrics::risk_return_ratio(eq),
            returns_volatility: metrics::returns_volatility(eq, periods_per_year),
            returns_skewness: metrics::returns_skewness(eq),
            returns_kurtosis: metrics::returns_kurtosis(eq),
            tail_ratio: metrics::tail_ratio(eq),
            omega: metrics::omega(eq, OMEGA_THRESHOLD),
            value_at_risk_95: metrics::value_at_risk(eq, TAIL_CONFIDENCE),
            expected_shortfall_95: metrics::expected_shortfall(eq, TAIL_CONFIDENCE),
        }
    }

    /// The value of one catalog id, or `None` when this struct does not hold it. The `match` is the
    /// seam between the roster in [`crate::metric_catalog`] and the fields here, and
    /// `every_extended_id_resolves_to_a_value` is what holds the two equal — a catalog row with no
    /// arm reddens rather than rendering nothing.
    pub fn value_of(&self, id: &str) -> Option<f64> {
        Some(match id {
            "net_profit" => self.net_profit,
            "gross_profit" => self.gross_profit,
            "gross_loss" => self.gross_loss,
            "total_fees" => self.total_fees,
            "avg_win" => self.avg_win,
            "avg_loss" => self.avg_loss,
            "largest_win" => self.largest_win,
            "largest_loss" => self.largest_loss,
            "payoff_ratio" => self.payoff_ratio,
            "expected_payoff" => self.expected_payoff,
            "consecutive_wins" => self.consecutive_wins as f64,
            "consecutive_losses" => self.consecutive_losses as f64,
            "sqn" => self.sqn,
            "long_ratio" => self.long_ratio,
            "sortino" => self.sortino,
            "calmar" => self.calmar,
            "cagr" => self.cagr,
            "mar_ratio" => self.mar_ratio,
            "recovery_factor" => self.recovery_factor,
            "ulcer_index" => self.ulcer_index,
            "ulcer_performance_index" => self.ulcer_performance_index,
            "k_ratio" => self.k_ratio,
            "risk_return_ratio" => self.risk_return_ratio,
            "returns_volatility" => self.returns_volatility,
            "returns_skewness" => self.returns_skewness,
            "returns_kurtosis" => self.returns_kurtosis,
            "tail_ratio" => self.tail_ratio,
            "omega" => self.omega,
            "value_at_risk_95" => self.value_at_risk_95,
            "expected_shortfall_95" => self.expected_shortfall_95,
            _ => return None,
        })
    }
}

/// What the run DEFERRED, skipped, could not price and refused — the counters that tell an
/// AMBIGUOUS result apart from a clean one.
///
/// # The defect this closes
///
/// Every one of these was already accumulated on [`BacktestResult`] and every one of them reached
/// the report through exactly one door: [`crate::zero_trade::ZeroTradeReport`], which
/// `crate::zero_trade::ZeroTradeReport::analyze` emits ONLY when the run closed no trades and its
/// equity never moved. So a run in which two fills in five were resolved by an intrabar coin-flip,
/// or in which a configured impact model priced nothing because the bars carried `volume = 0`,
/// reported a number indistinguishable from a run where none of that happened — and the counters
/// that would have said so were sitting on the result, dying when `run` returned.
///
/// # This is a MEASUREMENT channel, not a refusal
///
/// A non-zero counter is not automatically a fault: the opening fills of a run legitimately precede
/// a measurable impact window, and a session gate is supposed to skip a closed venue. What they buy
/// is the ability to ASK. Every field's own authority is the matching
/// [`BacktestResult`] field, which carries the argument for what its non-zero readings mean.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HonestyCounters {
    /// See [`BacktestResult::intrabar_both_hit`]: fills where stop and target were both inside one
    /// bar and the engine had to choose. The one counter whose non-zero reading is a statement
    /// about the DATA's resolution rather than about the configuration.
    pub intrabar_both_hit: u32,
    /// See [`BacktestResult::stale_deferrals`].
    pub stale_deferrals: u64,
    /// See [`BacktestResult::session_deferrals`].
    pub session_deferrals: u64,
    /// See [`BacktestResult::impact_unpriced`] — fills a CONFIGURED impact model charged nothing
    /// for. A count equal to the fill count means the model was never applied at all.
    pub impact_unpriced: u64,
    /// See [`BacktestResult::below_min_reversals`] — fills this backtest executed that the LIVE
    /// gate would have denied. Expected to stay zero above dust sizes; a non-zero reading falsifies
    /// that argument, which is the whole reason the counter exists.
    pub below_min_reversals: u64,
    /// The warm-up the run GATED on, in bars/ticks — the EFFECTIVE number, not `Strategy::warmup()`
    /// (see [`BacktestResult::warmup`]).
    pub warmup: usize,
    /// The gate-drop ledger aggregated by reason, first-seen order preserved — exactly
    /// [`crate::zero_trade::aggregate_denials`] over [`BacktestResult::dropped`].
    ///
    /// ⚠ **The aggregation is UNCONDITIONAL here, and that is the fix.** `aggregate_denials` is a
    /// free function and was always callable on any run; the only caller was
    /// `crate::zero_trade::ZeroTradeReport::analyze`, which is gated on a zero-trade flat-equity
    /// run — so the ledger of a run that traded 400 times and had 3,000 orders refused by the
    /// margin gate reached no document. Nothing about the function needed changing: the gate was
    /// never in it.
    pub denials: Vec<(String, u64)>,
}

impl HonestyCounters {
    /// Mirror the counters off a finished [`BacktestResult`]. A pure copy plus one
    /// [`crate::zero_trade::aggregate_denials`] fold — no counting, and nothing on any fill lane.
    pub fn from_result(r: &BacktestResult) -> Self {
        HonestyCounters {
            intrabar_both_hit: r.intrabar_both_hit,
            stale_deferrals: r.stale_deferrals,
            session_deferrals: r.session_deferrals,
            impact_unpriced: r.impact_unpriced,
            below_min_reversals: r.below_min_reversals,
            warmup: r.warmup,
            denials: crate::zero_trade::aggregate_denials(&r.dropped),
        }
    }

    /// Whether anything happened worth reporting. `false` is the ordinary clean run, and it is what
    /// keeps a normal human table byte-identical to before this block existed.
    ///
    /// ⚠ `warmup` is deliberately NOT part of this: every strategy with an indicator declares one,
    /// so counting it would make the block print on essentially every run and the signal would be
    /// gone. It is recorded because a zero-trade diagnosis needs it, not because it is an anomaly.
    pub fn is_noteworthy(&self) -> bool {
        self.intrabar_both_hit > 0
            || self.stale_deferrals > 0
            || self.session_deferrals > 0
            || self.impact_unpriced > 0
            || self.below_min_reversals > 0
            || !self.denials.is_empty()
    }
}

/// Serialize a metric that may carry the house `inf`/`0.0` sentinel (see
/// [`BacktestReport::profit_factor`]): non-finite values become JSON `null`.
///
/// NB serde_json maps non-finite floats to `null` on its own (only float MAP KEYS are an error
/// there), so this is NOT a rescue from a serialization failure — it PINS that mapping as the
/// documented contract, independent of the serializer backend (a format that hard-errors on
/// non-finite floats would otherwise break `--json` on a degenerate run).
fn ser_f64_null_when_nonfinite<S: serde::Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
    if v.is_finite() { s.serialize_f64(*v) } else { s.serialize_none() }
}

/// The inverse of [`ser_f64_null_when_nonfinite`]: JSON `null` becomes `f64::INFINITY`.
///
/// ⚠ **This is a MAPPING, not a guess, and only because of what the sentinel set is.**
/// [`crate::metrics::profit_factor`] answers `f64::INFINITY` when there are no losing trades and
/// some profit, `0.0` when there is neither, and a finite ratio otherwise — and `0.0` is FINITE, so
/// it serializes as `0.0`. `INFINITY` is therefore the only value that can ever have become `null`.
/// If a future metric with a DIFFERENT non-finite sentinel reuses the serializer, this inverse stops
/// being exact and the pair must be split.
fn de_f64_null_as_infinity<'de, D: serde::Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    Ok(Option::<f64>::deserialize(d)?.unwrap_or(f64::INFINITY))
}

impl BacktestReport {
    /// Compose a report from a raw [`BacktestResult`]. `periods_per_year` is the annualization
    /// factor `metrics::sharpe` needs — the caller (the `backtest` bin) picks it from the
    /// profile's data kind/interval via `harness::report::periods_per_year` (252 for daily bars; a
    /// documented default otherwise), since the result itself doesn't carry that information.
    pub fn from_result(name: Option<String>, r: &BacktestResult, periods_per_year: f64) -> Self {
        BacktestReport {
            name,
            final_equity: r.final_equity,
            total_return: metrics::total_return(&r.equity_curve),
            n_trades: r.n_trades,
            win_rate: metrics::win_rate(&r.trades),
            sharpe: metrics::sharpe(&r.equity_curve, periods_per_year),
            max_drawdown: metrics::max_drawdown(&r.equity_curve),
            profit_factor: metrics::profit_factor(&r.trades),
            funding_paid: r.funding_paid,
            per_symbol_pnl: r.per_symbol_pnl.clone(),
            // `None` for any run that traded or moved equity -> the report is byte-identical to
            // before this field existed; `Some` only diagnoses the all-zero / flat case.
            zero_trade: crate::zero_trade::ZeroTradeReport::analyze(r),
            // ⚠ ALWAYS `Some` from this door, unlike `zero_trade`. A conditional long-form block
            // would make "this run was clean" and "this run was recorded by an older binary"
            // the same bytes, and the second is the one a reader must refuse rather than render.
            extended: Some(ExtendedMetrics::from_result(r, periods_per_year)),
            honesty: Some(HonestyCounters::from_result(r)),
            // The stamp needs the profile, which this crate cannot name — see the field's doc.
            realism: None,
        }
    }

    /// Attach the cost model this run ran under. The producer's door, called once beside
    /// [`Self::from_result`] — see [`Self::realism`] for why it is a second step rather than a
    /// parameter.
    #[must_use]
    pub fn with_realism(mut self, stamp: crate::realism::RealismStamp) -> Self {
        self.realism = Some(stamp);
        self
    }

    /// The value of one [`crate::metric_catalog::METRICS`] id, wherever it is stored.
    ///
    /// Three outcomes, never two, and the middle one is the point: `Some(v)` is the number,
    /// `None` with [`Self::extended`] absent means the RUN never recorded it, and a `None` for an
    /// id the catalog does not hold cannot happen because
    /// [`crate::metric_catalog::parse_metric_selection`] already refused it. A caller that collapses
    /// the first two renders `0.0` for a run that has no answer.
    pub fn metric_value(&self, id: &str) -> Option<f64> {
        match id {
            "final_equity" => Some(self.final_equity),
            "total_return" => Some(self.total_return),
            "n_trades" => Some(self.n_trades as f64),
            "win_rate" => Some(self.win_rate),
            "sharpe" => Some(self.sharpe),
            "max_drawdown" => Some(self.max_drawdown),
            "profit_factor" => Some(self.profit_factor),
            "funding_paid" => Some(self.funding_paid),
            other => self.extended.as_ref().and_then(|e| e.value_of(other)),
        }
    }

    /// Render what an operator selected.
    ///
    /// [`crate::metric_catalog::MetricSelection::Compact`] delegates to this type's `Display`
    /// rather than re-rendering the same eight rows: the compact table is a published human format
    /// with a zero-trade branch and two conditional rows in it, and a second spelling of it would
    /// be two formats that drift. Every other selection renders one aligned row per id.
    ///
    /// A selection this report cannot answer produces a row saying so BY NAME rather than a zero or
    /// a silent omission — the run is what is missing, not the metric.
    ///
    /// ⚠ **This function owns the LAYOUT and nothing else.** How a number reads — the percent
    /// scaling, the decimal count — belongs to
    /// [`crate::metric_catalog::MetricUnit::render`], because it used to be spelled here AND in
    /// three other renderers, and the ones that spelled it differently published a fraction as a
    /// percent. Do not re-derive a cell here; widen the unit.
    ///
    /// # Nothing outside this file calls it, so four of the five selections have no spelling
    ///
    /// ⚠ MEASURED: every `render_metrics` call site in the tree is inside this file's own
    /// `#[cfg(test)] mod tests`. The only production use of the selection type anywhere is
    /// `crates/vike-cli/src/cmd/runs/show.rs`'s `report_key_order`, which asks
    /// [`crate::metric_catalog::MetricSelection::Compact`] for its ids in order to order JSON KEYS
    /// and never renders a cell. `--metrics` is still `value: Value::None` in
    /// `crates/vike-cli/src/surface.rs`'s `FLAGS`, so `Full`, `Named`, `Honesty` and `Realism` have
    /// no spelling an operator can type — the door
    /// [`crate::metric_catalog::parse_metric_selection`] was written for, whose own doc carries
    /// what wiring it owes.
    ///
    /// ⚠ **What that does NOT mean, and the stronger claim is the tempting one: it does NOT mean
    /// the honesty counters and the realism stamp go unprinted.** Two shipped surfaces print them
    /// today. This type's own `Display` prints the honesty block whenever
    /// [`HonestyCounters::is_noteworthy`] and a one-line `realism: FRICTIONLESS` row whenever the
    /// stamp carries that reason — both deliberately conditional, argued at those two sites. And
    /// `crates/vike-cli/src/cmd/runs/show.rs`'s `show_text` prints every `report.json` key its
    /// `report_key_order` does not name, alphabetically, which is `extended`, `honesty` and
    /// `realism` as their raw JSON. So what has no door is this RENDERER and the selection plane
    /// around it: the aligned per-id table, the two keywords that render those blocks instead of
    /// their JSON, and the run-aware "not recorded" sentences below — which are the half that
    /// distinguishes "nothing happened" from "nobody looked", and the reason wiring the door is
    /// worth doing rather than merely tidy.
    pub fn render_metrics(&self, sel: &crate::metric_catalog::MetricSelection) -> String {
        use crate::metric_catalog::{MetricSelection, MetricUnit, spec_for};

        match sel {
            MetricSelection::Compact => return self.to_string(),
            MetricSelection::Honesty => {
                return match &self.honesty {
                    Some(h) => h.to_string(),
                    None => "honesty counters: not recorded — this run predates the block, so \
                             \"nothing happened\" and \"nobody looked\" are not distinguishable \
                             for it. Re-run the profile.\n"
                        .to_string(),
                };
            }
            MetricSelection::Realism => {
                return match &self.realism {
                    Some(r) => r.to_string(),
                    None => "realism stamp: not recorded — nobody stamped this run, which is a \
                             different answer from a frictionless one and must not be read as \
                             one. Re-run the profile.\n"
                        .to_string(),
                };
            }
            MetricSelection::Full | MetricSelection::Named(_) => {}
        }

        let ids = sel.ids();
        let width = ids.iter().map(|i| i.len()).max().unwrap_or(0);
        let mut out = String::with_capacity(64 * ids.len() + 128);
        // The label column is `width` wide for every row INCLUDING this one, so a reader's eye and
        // a `cut -c` both find the values in one place.
        let _ = writeln!(out, "{:width$}  {}", "name", self.name.as_deref().unwrap_or("(unnamed)"));
        for id in ids {
            // `spec_for` cannot answer `None` for an id that came out of `sel.ids()` — that
            // function filters `METRICS` itself — so the fallback is unreachable rather than a
            // silent default.
            let unit = spec_for(id).map_or(MetricUnit::Ratio, |s| s.unit);
            match self.metric_value(id) {
                Some(v) => {
                    // ⚠ The scaling and the precision are the UNIT's, not this renderer's — see
                    // [`crate::metric_catalog::MetricUnit::render`]. This was a private `match`
                    // here, the second of four spellings of one rule, and the doors that spelled it
                    // differently published a percent row a hundred times too small.
                    let rendered = unit.render(v);
                    let _ = writeln!(out, "{id:width$}  {rendered}");
                }
                None => {
                    let _ = writeln!(
                        out,
                        "{id:width$}  not recorded (this run has no `extended` block)"
                    );
                }
            }
        }
        out
    }
}

impl fmt::Display for HonestyCounters {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "intrabar_both_hit:   {}", self.intrabar_both_hit)?;
        writeln!(f, "stale_deferrals:     {}", self.stale_deferrals)?;
        writeln!(f, "session_deferrals:   {}", self.session_deferrals)?;
        writeln!(f, "impact_unpriced:     {}", self.impact_unpriced)?;
        writeln!(f, "below_min_reversals: {}", self.below_min_reversals)?;
        writeln!(f, "warmup:              {}", self.warmup)?;
        if self.denials.is_empty() {
            writeln!(f, "denials:             (none)")?;
        } else {
            writeln!(f, "denials:")?;
            for (reason, count) in &self.denials {
                writeln!(f, "  {reason}: {count}")?;
            }
        }
        Ok(())
    }
}

impl fmt::Display for BacktestReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // ⚠ **The value cells come from the UNIT, and the substitution is BYTE-IDENTICAL to the
        // `{:.4}%` / `{:.2}` / `{:.4}` literals that used to be spelled here.**
        // `MetricUnit::Percent::render` IS `format!("{:.4}%", v * 100.0)` — the same expression in
        // the same order — and `Money`/`Ratio` are the same for `{:.2}`/`{:.4}`. So no fixture and
        // no operator's muscle memory moves; what changes is that this published table can no
        // longer disagree with `--metrics full` about what a percent is, which it could while both
        // spelled the rule for themselves. `the_compact_table_cells_are_the_units_own_renderings`
        // is the pin.
        //
        // `n_trades` deliberately keeps its integer `{}` rather than routing through
        // `MetricUnit::Count`: the catalog renders a count as the `f64` that
        // `ExtendedMetrics::value_of` widens it to, and casting a `usize` here to print it would
        // buy nothing and lose exactness above 2^53.
        use crate::metric_catalog::MetricUnit;

        writeln!(f, "name:          {}", self.name.as_deref().unwrap_or("(unnamed)"))?;
        // A zero-trade / flat-equity run prints its DIAGNOSIS instead of the bare all-zero metrics
        // table. `zero_trade` is `Some` only for such a run (see `ZeroTradeReport::analyze`), so a
        // run with trades falls straight through to the unchanged table below and is byte-identical.
        if let Some(zt) = &self.zero_trade {
            writeln!(f, "final_equity:  {}", MetricUnit::Money.render(self.final_equity))?;
            writeln!(f, "n_trades:      0")?;
            write!(f, "{zt}")?;
            return Ok(());
        }
        writeln!(f, "final_equity:  {}", MetricUnit::Money.render(self.final_equity))?;
        writeln!(f, "total_return:  {}", MetricUnit::Percent.render(self.total_return))?;
        writeln!(f, "n_trades:      {}", self.n_trades)?;
        writeln!(f, "win_rate:      {}", MetricUnit::Percent.render(self.win_rate))?;
        writeln!(f, "sharpe:        {}", MetricUnit::Ratio.render(self.sharpe))?;
        writeln!(f, "max_drawdown:  {}", MetricUnit::Percent.render(self.max_drawdown))?;
        // Funding P&L: shown ONLY when nonzero, so a spot / no-funding report is byte-identical to
        // before this field existed (same discipline as the omitted profit_factor row).
        if self.funding_paid != 0.0 {
            writeln!(f, "funding_paid:  {}", MetricUnit::Money.render(self.funding_paid))?;
        }
        // ⚠ **A FRICTIONLESS run says so on the human table, and nothing else new does.** The
        // stamp itself is a whole block and belongs behind `--metrics realism`; this one line is
        // here because the harm it answers is specific: `fee_rate` and `slippage` both default to
        // `0.0`, so an unfinished profile and a deliberately costless one printed identical tables,
        // and the first is the one somebody acts on. A COSTED run adds nothing, so every existing
        // report that charged anything is byte-identical.
        if let Some(why) = self.realism.as_ref().and_then(|r| r.frictionless.as_deref()) {
            writeln!(f, "realism:       FRICTIONLESS — {why}")?;
        }
        // Same discipline as `funding_paid` above: printed only when something actually happened,
        // so a clean run's table does not move. `is_noteworthy` deliberately excludes `warmup` —
        // see its doc.
        if let Some(h) = self.honesty.as_ref().filter(|h| h.is_noteworthy()) {
            writeln!(f, "honesty:")?;
            for line in h.to_string().lines() {
                writeln!(f, "  {line}")?;
            }
        }
        if self.per_symbol_pnl.is_empty() {
            writeln!(f, "per_symbol_pnl: (none)")?;
        } else {
            writeln!(f, "per_symbol_pnl:")?;
            for (sym, pnl) in &self.per_symbol_pnl {
                writeln!(f, "  {sym}: {pnl:.2}")?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::Trade;

    fn trade(pnl: f64) -> Trade {
        Trade {
            entry_price: 100.0,
            exit_price: 100.0 + pnl,
            size: 1.0,
            pnl,
            fees: 0.0,
            entry_ts: 0,
            exit_ts: 1,
            symbol: "BTCUSDT".to_string(),
            mae: 0.0,
            mfe: 0.0,
            is_long: true,
        }
    }

    fn sample_result() -> BacktestResult {
        BacktestResult {
            trades: vec![trade(10.0), trade(-5.0)],
            equity_curve: vec![1000.0, 1010.0, 990.0, 1005.0],
            final_equity: 1005.0,
            n_trades: 2,
            intrabar_both_hit: 0,
            per_symbol_pnl: vec![("BTCUSDT".to_string(), 5.0)],
            per_symbol_curves: Vec::new(),
            equity_ts: Vec::new(),
            stale_deferrals: 0,
            impact_unpriced: 0,
            session_deferrals: 0,
            dropped: Vec::new(),
            below_min_reversals: 0,
            warmup: 0,
            funding_paid: 0.0,
            maker_fills: 0,
            taker_fills: 2,
            fees_paid: 0.0,
        }
    }

    #[test]
    fn matches_direct_metrics_calls() {
        let r = sample_result();
        let report = BacktestReport::from_result(Some("demo".to_string()), &r, 252.0);

        assert_eq!(report.name.as_deref(), Some("demo"));
        assert_eq!(report.final_equity, r.final_equity);
        assert_eq!(report.n_trades, r.n_trades);
        assert_eq!(report.per_symbol_pnl, r.per_symbol_pnl);
        assert_eq!(report.total_return, metrics::total_return(&r.equity_curve));
        assert_eq!(report.max_drawdown, metrics::max_drawdown(&r.equity_curve));
        assert_eq!(report.win_rate, metrics::win_rate(&r.trades));
        assert_eq!(report.sharpe, metrics::sharpe(&r.equity_curve, 252.0));
        assert_eq!(report.profit_factor, metrics::profit_factor(&r.trades));

        // Sanity on the composed values themselves, not just that they match — a report that
        // merely "matches metrics" by both being wrong the same way would still pass the
        // assertions above.
        assert_eq!(report.win_rate, 0.5); // 1 win / 2 trades
        assert!((report.total_return - 0.005).abs() < 1e-12); // 1005/1000 - 1
        assert!((report.max_drawdown - (1010.0 - 990.0) / 1010.0).abs() < 1e-12);
    }

    #[test]
    fn json_round_trips() {
        let r = sample_result();
        let report = BacktestReport::from_result(Some("demo".to_string()), &r, 252.0);

        let json = serde_json::to_string(&report).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        // serde_json's default (non-`float_roundtrip`) float parser is not always bit-exact on
        // the way back in — a documented serde_json limitation, not a report bug — so float
        // fields are compared with a tight relative tolerance (matches this crate's own
        // convention elsewhere: metrics.rs's module doc gates sqrt/pow-derived values at
        // ≤1e-12 relative for the same reason). Exact types (strings/counts) compare exactly.
        let approx = |got: f64, want: f64| {
            assert!((got - want).abs() <= want.abs() * 1e-9 + 1e-12, "got {got}, want {want}");
        };
        assert_eq!(parsed["name"], "demo");
        approx(parsed["final_equity"].as_f64().unwrap(), report.final_equity);
        assert_eq!(parsed["n_trades"], report.n_trades as u64);
        approx(parsed["win_rate"].as_f64().unwrap(), report.win_rate);
        approx(parsed["sharpe"].as_f64().unwrap(), report.sharpe);
        approx(parsed["max_drawdown"].as_f64().unwrap(), report.max_drawdown);
        approx(parsed["total_return"].as_f64().unwrap(), report.total_return);
        assert_eq!(parsed["per_symbol_pnl"][0][0], "BTCUSDT");
        approx(parsed["per_symbol_pnl"][0][1].as_f64().unwrap(), 5.0);
        approx(parsed["profit_factor"].as_f64().unwrap(), report.profit_factor);
        // 10/5 = 2.0
    }

    /// The house `inf` sentinel (`profit_factor` with no losing trades) serializes as `null`.
    /// This pins the CONTRACT a `--json` consumer sees, not a failure mode it rescues: serde_json
    /// would emit `null` for a non-finite float anyway (see [`ser_f64_null_when_nonfinite`]) — the
    /// point is that the shape is `null`, never a `NaN`/`inf` token and never an error.
    #[test]
    fn nonfinite_profit_factor_serializes_as_null() {
        let mut r = sample_result();
        r.trades = vec![trade(10.0)]; // wins only -> profit_factor = inf
        let report = BacktestReport::from_result(None, &r, 252.0);
        assert!(report.profit_factor.is_infinite());

        let json = serde_json::to_string(&report).expect("inf sentinel must not break JSON");
        assert!(!json.contains("inf"), "no bare inf token (invalid JSON): {json}");
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed["profit_factor"].is_null());
    }

    #[test]
    fn display_is_a_compact_human_table() {
        let r = sample_result();
        let report = BacktestReport::from_result(None, &r, 252.0);
        let s = report.to_string();
        assert!(s.contains("(unnamed)"));
        assert!(s.contains("final_equity:"));
        assert!(s.contains("win_rate:"));
        assert!(s.contains("sharpe:"));
        assert!(s.contains("max_drawdown:"));
        assert!(s.contains("BTCUSDT"));
    }

    /// OFF / byte-identical path: a run WITH trades is never diagnosed, so its report grows no
    /// `zero_trade` field in JSON and its `Display` is the unchanged metrics table.
    #[test]
    fn zero_trade_is_absent_and_output_unchanged_for_a_run_with_trades() {
        let r = sample_result(); // n_trades = 2, moving equity curve
        let report = BacktestReport::from_result(Some("demo".to_string()), &r, 252.0);
        assert!(report.zero_trade.is_none(), "a run with trades is never diagnosed");

        // JSON: no `zero_trade` key (skip_serializing_if) -> byte-identical to before the field.
        let json = serde_json::to_string(&report).unwrap();
        assert!(
            !json.contains("zero_trade"),
            "a normal-run JSON must not grow a zero_trade field: {json}"
        );

        // Display: the full metrics table (the diagnosis branch is skipped).
        let s = report.to_string();
        assert!(s.contains("win_rate:"));
        assert!(s.contains("sharpe:"));
        assert!(s.contains("max_drawdown:"));
        assert!(!s.contains("probable cause"));
    }

    /// A contrived zero-trade / flat-equity run surfaces the correct ranked cause: its `Display`
    /// prints the diagnosis INSTEAD OF the all-zero metrics rows, and its JSON carries `zero_trade`.
    #[test]
    fn zero_trade_diagnosis_replaces_the_table_for_a_flat_zero_trade_run() {
        let r = BacktestResult {
            n_trades: 0,
            equity_curve: vec![1000.0, 1000.0, 1000.0],
            final_equity: 1000.0,
            dropped: vec![
                ("BTCUSDT".to_string(), "insufficient-margin".to_string(), 1.0, 0.0),
                ("BTCUSDT".to_string(), "insufficient-margin".to_string(), 2.0, 0.0),
            ],
            ..Default::default()
        };
        let report = BacktestReport::from_result(None, &r, 252.0);

        let zt = report.zero_trade.as_ref().expect("a flat zero-trade run must be diagnosed");
        assert_eq!(zt.causes[0].code, "orders-denied");

        // Display: diagnosis present, the metric rows (win_rate/sharpe) replaced by it.
        let s = report.to_string();
        assert!(s.contains("probable cause"));
        assert!(s.contains("rejected before filling"));
        assert!(!s.contains("win_rate:"), "the bare metrics table must be replaced: {s}");
        assert!(!s.contains("sharpe:"));

        // JSON now carries the diagnosis.
        let json = serde_json::to_string(&report).unwrap();
        assert!(
            json.contains("zero_trade"),
            "a zero-trade run's JSON carries the diagnosis: {json}"
        );
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["zero_trade"]["causes"][0]["code"], "orders-denied");
    }

    /// The daily anchor is preserved BIT-for-bit — the whole reason the intraday fix could ship
    /// without moving a single existing daily report.
    #[test]
    fn the_daily_anchor_is_preserved_exactly() {
        assert_eq!(periods_per_year_for_interval("1d"), DAILY_PERIODS_PER_YEAR);
    }

    /// The two values the walk-forward divergence was measured at: `1h` is the interval the
    /// Studio door reported `sqrt(24)` low, `1m` the one the CI roundtrip fixture uses and the
    /// original intraday bug understated by `sqrt(1440)`.
    #[test]
    fn intraday_intervals_scale_off_the_daily_anchor() {
        assert_eq!(periods_per_year_for_interval("1h"), 6_048.0);
        assert_eq!(periods_per_year_for_interval("1m"), 362_880.0);
        // ...and the scale is exactly "how many of that interval fit in a day".
        assert_eq!(periods_per_year_for_interval("4h"), DAILY_PERIODS_PER_YEAR * 6.0);
    }

    /// An interval the parser cannot read falls back rather than fabricating a scale. This is a
    /// reporting knob, not a validation site.
    #[test]
    fn an_unparseable_interval_falls_back_instead_of_fabricating_a_scale() {
        assert_eq!(periods_per_year_for_interval(""), DEFAULT_PERIODS_PER_YEAR);
        assert_eq!(periods_per_year_for_interval("not-an-interval"), DEFAULT_PERIODS_PER_YEAR);
    }

    /// A report with nothing degenerate in it — the base the sentinel tests perturb one field of.
    fn a_finite_report() -> BacktestReport {
        BacktestReport {
            name: None,
            final_equity: 100_000.0,
            total_return: 0.0,
            n_trades: 0,
            win_rate: 0.0,
            sharpe: 0.0,
            max_drawdown: 0.0,
            profit_factor: 0.0,
            funding_paid: 0.0,
            per_symbol_pnl: Vec::new(),
            zero_trade: None,
            extended: None,
            honesty: None,
            realism: None,
        }
    }

    /// ⚠ **The round trip a stored run needs.** `report.json` has been a write-only document: this
    /// type derived `Serialize` and nothing else, so a `show`, a `diff` or a `gate` verb could not
    /// read back the numbers this binary itself wrote. Asserted through the SERIALIZED FORM rather
    /// than through a struct clone, because the bytes on disk are what a reader holds.
    #[test]
    fn a_serialized_report_reads_back_into_the_same_values() {
        let written = BacktestReport {
            name: Some("sma cross".to_string()),
            final_equity: 100_009.8,
            total_return: 0.000098,
            n_trades: 3,
            win_rate: 0.6667,
            sharpe: 1.25,
            max_drawdown: 0.031,
            profit_factor: 2.5,
            funding_paid: -1.5,
            per_symbol_pnl: vec![("BTCUSDT".to_string(), 10.0)],
            zero_trade: None,
            extended: None,
            honesty: None,
            realism: None,
        };

        let json = serde_json::to_string(&written).unwrap();
        let back: BacktestReport = serde_json::from_str(&json).unwrap();

        assert_eq!(back.name.as_deref(), Some("sma cross"));
        assert_eq!(back.final_equity, 100_009.8);
        assert_eq!(back.total_return, 0.000098);
        assert_eq!(back.n_trades, 3);
        assert_eq!(back.win_rate, 0.6667);
        assert_eq!(back.sharpe, 1.25);
        assert_eq!(back.max_drawdown, 0.031);
        assert_eq!(back.profit_factor, 2.5);
        assert_eq!(back.funding_paid, -1.5);
        assert_eq!(back.per_symbol_pnl, vec![("BTCUSDT".to_string(), 10.0)]);
        assert!(back.zero_trade.is_none(), "an absent key must read as None, not fail the parse");
    }

    /// The house `inf` sentinel survives the round trip EXACTLY, and it can: `metrics::profit_factor`
    /// answers `f64::INFINITY` when there are no losing trades and some profit, `0.0` when there is
    /// neither — and `0.0` is finite, so it serializes as `0.0`. `INFINITY` is the ONLY value that
    /// ever becomes `null`, which is what makes `null -> INFINITY` a mapping rather than a guess.
    #[test]
    fn an_infinite_profit_factor_round_trips_through_its_json_null() {
        let written = BacktestReport { profit_factor: f64::INFINITY, ..a_finite_report() };

        let json = serde_json::to_string(&written).unwrap();
        assert!(json.contains("\"profit_factor\":null"), "the wire shape must not change: {json}");

        let back: BacktestReport = serde_json::from_str(&json).unwrap();
        assert!(back.profit_factor.is_infinite() && back.profit_factor.is_sign_positive());
    }

    /// A zero-trade run's DIAGNOSIS is the half a reader most needs and the half that would have
    /// been lost first: it is an `Option` skipped on serialization, so it exercises both the
    /// `default` and the nested `Deserialize` this task adds.
    #[test]
    fn a_zero_trade_diagnosis_reads_back_with_its_ranked_causes() {
        let written = BacktestReport {
            zero_trade: Some(crate::zero_trade::ZeroTradeReport {
                causes: vec![crate::zero_trade::ZeroTradeCause {
                    code: "no-data".to_string(),
                    headline: "the data slice was empty".to_string(),
                    detail: "check [data].from/to against what the store holds".to_string(),
                }],
            }),
            ..a_finite_report()
        };

        let json = serde_json::to_string(&written).unwrap();
        let back: BacktestReport = serde_json::from_str(&json).unwrap();

        let causes = back.zero_trade.expect("the diagnosis must survive").causes;
        assert_eq!(causes.len(), 1);
        assert_eq!(causes[0].code, "no-data");
        assert_eq!(causes[0].headline, "the data slice was empty");
    }

    /// ⚠ **THE BACK-COMPATIBILITY PROOF for the SECOND persisted document**, and the reason the
    /// four fields added LATER carry `#[serde(default)]` (and the seven originals do not).
    ///
    /// Written as raw TEXT rather than through the serializer, deliberately and for the same reason
    /// `vike_model::runs`'s manifest twin is: the bytes already on somebody's disk are what this
    /// test is about, and a round trip through the CURRENT struct can never see a field the old
    /// writer did not emit. The document below is a `report.json` from before `profit_factor`,
    /// `funding_paid`, `per_symbol_pnl` and `zero_trade` existed — the four this file's own field
    /// docs record as added later.
    #[test]
    fn a_report_written_before_the_later_fields_existed_still_reads() {
        let old_on_disk = r#"{
  "name": "sma cross",
  "final_equity": 100009.8,
  "total_return": 0.000098,
  "n_trades": 3,
  "win_rate": 0.6667,
  "sharpe": 1.25,
  "max_drawdown": 0.031
}
"#;

        let back: BacktestReport =
            serde_json::from_str(old_on_disk).expect("an old report.json must still load");

        assert_eq!(back.name.as_deref(), Some("sma cross"));
        assert_eq!(back.final_equity, 100_009.8, "and every field it DID carry is untouched");
        assert_eq!(back.n_trades, 3);
        assert_eq!(back.sharpe, 1.25);
        assert_eq!(back.profit_factor, 0.0, "absent is `no meaningful ratio`, not a parse failure");
        assert_eq!(back.funding_paid, 0.0);
        assert!(back.per_symbol_pnl.is_empty());
        assert!(back.zero_trade.is_none());
    }

    /// The seven fields that have existed since `report.json`'s first version, as `(key, value)`.
    /// The four NOT here — `profit_factor`, `funding_paid`, `per_symbol_pnl`, `zero_trade` — are
    /// the ones this file's own field docs record as added later, and the only ones that default.
    const ORIGINAL_KEYS: &[(&str, &str)] = &[
        ("name", "null"),
        ("final_equity", "1.0"),
        ("total_return", "0.0"),
        ("n_trades", "0"),
        ("win_rate", "0.0"),
        ("sharpe", "0.0"),
        ("max_drawdown", "0.0"),
    ];

    /// The oldest shape a `report.json` ever had, optionally with one key removed and optionally
    /// with LATER keys appended — a COMPLETE document either way, which is what the required
    /// originals now oblige every fixture to be.
    fn oldest_report_with(without: Option<&str>, extra: &[(&str, &str)]) -> String {
        let body: Vec<String> = ORIGINAL_KEYS
            .iter()
            .filter(|(k, _)| Some(*k) != without)
            .chain(extra.iter())
            .map(|(k, v)| format!("  \"{k}\": {v}"))
            .collect();
        format!("{{\n{}\n}}\n", body.join(",\n"))
    }

    /// [`oldest_report_with`] with no later keys.
    fn oldest_report(without: Option<&str>) -> String {
        oldest_report_with(without, &[])
    }

    /// ⚠ **THE RULE, from both sides: the four LATER fields default and the seven ORIGINALS do
    /// not.** The first half is back-compatibility. The second is the only structural check that
    /// the bytes are a report AT ALL — with everything optional, `{}` and any unrelated JSON object
    /// deserialize into a real-looking all-zeros run, which `show`/`diff`/`gate` would render and
    /// compare against a baseline. Pre-derive that was a parse error; it stays one.
    #[test]
    fn only_the_four_later_fields_default_and_a_document_that_is_not_a_report_is_refused() {
        let back: BacktestReport = serde_json::from_str(&oldest_report(None))
            .expect("the four LATER fields must default, or every old report.json stops loading");
        assert_eq!(back.profit_factor, 0.0, "absent is `no meaningful ratio`");
        assert_eq!(back.funding_paid, 0.0);
        assert!(back.per_symbol_pnl.is_empty());
        assert!(back.zero_trade.is_none());
        assert_eq!(back.final_equity, 1.0, "and what it DID carry is untouched");

        assert!(
            serde_json::from_str::<BacktestReport>("{}").is_err(),
            "an EMPTY object must not deserialize into an all-zeros run a reader would compare"
        );
        assert!(
            serde_json::from_str::<BacktestReport>(r#"{ "unrelated": 1 }"#).is_err(),
            "nor must an unrelated JSON object"
        );

        // Per ORIGINAL SCALAR, because one `default` slipping back in is exactly what this
        // catches.
        //
        // ⚠ `name` is EXCLUDED and that is serde's rule rather than this type's: a bare
        // `Option<T>` field is optional whatever attributes it carries, because serde's own
        // `missing_field` helper deserializes an absent key through a unit deserializer and
        // `Option` answers `None` to it. Removing `#[serde(default)]` from `name` therefore
        // changed nothing, which is exactly why asserting it here would pin a property this crate
        // does not control. The six scalars below are what make a non-report REFUSED, and they are
        // enough: `{}` fails on the first of them.
        for (key, _) in ORIGINAL_KEYS.iter().filter(|(k, _)| *k != "name") {
            let without = oldest_report(Some(key));
            assert!(
                serde_json::from_str::<BacktestReport>(&without).is_err(),
                "a report missing the ORIGINAL field `{key}` must be a PARSE FAILURE, not a \
                 silent zero:\n{without}"
            );
        }
    }

    /// The `inf` sentinel still routes through the custom deserializer when the key is PRESENT —
    /// `default` and `deserialize_with` answer different questions and both are needed.
    #[test]
    fn a_present_null_profit_factor_is_still_the_infinity_sentinel() {
        // ⚠ A COMPLETE document, because the seven ORIGINAL fields are required now — a bare
        // `{ "profit_factor": null }` is correctly a parse failure, which is the whole point of
        // narrowing the defaults.
        let doc = oldest_report_with(None, &[("profit_factor", "null")]);

        let back: BacktestReport = serde_json::from_str(&doc).unwrap();

        assert!(back.profit_factor.is_infinite() && back.profit_factor.is_sign_positive());
    }

    // --- the long-form catalog, the honesty counters and the realism stamp ---------------------

    /// The same property `matches_direct_metrics_calls` asserts for the compact eight, for the
    /// thirty that joined them: every field is one `metrics::` call and nothing here is new math,
    /// so a divergence is a wiring bug.
    #[test]
    fn extended_matches_direct_metrics_calls() {
        let r = sample_result();
        let e = ExtendedMetrics::from_result(&r, 252.0);
        let eq = &r.equity_curve;
        let tr = &r.trades;

        assert_eq!(e.net_profit, metrics::net_profit(tr));
        assert_eq!(e.gross_profit, metrics::gross_profit(tr));
        assert_eq!(e.gross_loss, metrics::gross_loss(tr));
        assert_eq!(e.total_fees, metrics::total_fees(tr));
        assert_eq!(e.avg_win, metrics::avg_win(tr));
        assert_eq!(e.avg_loss, metrics::avg_loss(tr));
        assert_eq!(e.largest_win, metrics::largest_win(tr));
        assert_eq!(e.largest_loss, metrics::largest_loss(tr));
        assert_eq!(e.payoff_ratio, metrics::payoff_ratio(tr));
        assert_eq!(e.expected_payoff, metrics::expected_payoff(tr));
        assert_eq!(e.consecutive_wins, metrics::consecutive_wins(tr));
        assert_eq!(e.consecutive_losses, metrics::consecutive_losses(tr));
        assert_eq!(e.sqn, metrics::sqn(tr));
        assert_eq!(e.long_ratio, metrics::long_ratio(tr));
        assert_eq!(e.sortino, metrics::sortino(eq, 252.0));
        assert_eq!(e.calmar, metrics::calmar(eq, 252.0));
        assert_eq!(e.cagr, metrics::cagr(eq, 252.0));
        assert_eq!(e.mar_ratio, metrics::mar_ratio(eq, 252.0));
        assert_eq!(e.recovery_factor, metrics::recovery_factor(eq));
        assert_eq!(e.ulcer_index, metrics::ulcer_index(eq));
        assert_eq!(e.ulcer_performance_index, metrics::ulcer_performance_index(eq, 252.0));
        assert_eq!(e.k_ratio, metrics::k_ratio(eq));
        assert_eq!(e.risk_return_ratio, metrics::risk_return_ratio(eq));
        assert_eq!(e.returns_volatility, metrics::returns_volatility(eq, 252.0));
        assert_eq!(e.returns_skewness, metrics::returns_skewness(eq));
        assert_eq!(e.returns_kurtosis, metrics::returns_kurtosis(eq));
        assert_eq!(e.tail_ratio, metrics::tail_ratio(eq));
        assert_eq!(e.omega, metrics::omega(eq, OMEGA_THRESHOLD));
        assert_eq!(e.value_at_risk_95, metrics::value_at_risk(eq, TAIL_CONFIDENCE));
        assert_eq!(e.expected_shortfall_95, metrics::expected_shortfall(eq, TAIL_CONFIDENCE));
    }

    /// ⚠ **The seam the catalog exists to hold, gated BOTH ways.** A row in
    /// [`crate::metric_catalog::METRICS`] with no field behind it renders "not recorded" forever on
    /// a report that recorded everything, and a field with no row is a number nobody can ask for.
    /// Both are silent, which is why this is a test rather than a convention.
    #[test]
    fn every_catalog_id_resolves_and_every_stored_metric_is_in_the_catalog() {
        use crate::metric_catalog::{METRICS, MetricHome, MetricSelection};

        let report = BacktestReport::from_result(None, &sample_result(), 252.0);
        for m in METRICS {
            assert!(
                report.metric_value(m.id).is_some(),
                "catalog id `{}` resolves to no value on a fully-composed report — either the \
                 `metric_value`/`value_of` arm is missing or the row names a field that does not \
                 exist",
                m.id
            );
        }

        // ...and the other direction, through the serialized form: every key of the `extended`
        // block must be a catalog row. Done on the JSON rather than on the struct because Rust
        // cannot enumerate fields, which is the same reason
        // `crates/vike-backtest/tests/run_record_completeness.rs` reads its type as text.
        let block = serde_json::to_value(report.extended.as_ref().unwrap()).unwrap();
        for key in block.as_object().expect("the extended block is an object").keys() {
            let spec = crate::metric_catalog::spec_for(key);
            assert!(spec.is_some(), "`extended.{key}` is stored and no catalog row names it");
            assert_eq!(
                spec.unwrap().home,
                MetricHome::Extended,
                "`{key}` is stored in the extended block and its catalog row says otherwise"
            );
        }

        // The selection every consumer will actually ask for must render every row.
        let rendered = report.render_metrics(&MetricSelection::Full);
        for m in METRICS {
            assert!(rendered.contains(m.id), "`--metrics full` omits {}", m.id);
        }
        // ⚠ Asserted over the WHOLE rendering rather than per id, and deliberately: the rows are
        // column-padded, so a per-id `format!("{id} not recorded")` needle can never match and the
        // per-id spelling of this check was VACUOUS — it passed whatever the renderer did.
        assert!(
            !rendered.contains("not recorded"),
            "`--metrics full` reports an unrecorded metric on a report that recorded every one:\n\
             {rendered}"
        );
    }

    /// ⚠ **The renderer DELEGATES its cells rather than re-deciding them.** `render_metrics`
    /// carried its own private `match` over `crate::metric_catalog::MetricUnit` — the second of
    /// four spellings of one rule — and this asserts that every row it prints is byte-identical to
    /// what the unit itself answers. So an edit to one can no longer move the other, which is the
    /// drift that let `crates/vike-report/src/html.rs`'s `render_html_inner` render two MONEY rows
    /// at four decimals while this door rendered them at two.
    #[test]
    fn every_rendered_row_is_the_units_own_rendering() {
        use crate::metric_catalog::{METRICS, MetricSelection, spec_for};

        let report = BacktestReport::from_result(None, &sample_result(), 252.0);
        let rendered = report.render_metrics(&MetricSelection::Full);
        for m in METRICS {
            let v = report.metric_value(m.id).expect("a fully-composed report answers every id");
            let cell = spec_for(m.id).expect("the id came out of METRICS").unit.render(v);
            // Matched as "the line that starts with this id ends with this cell", because the rows
            // are column-padded: a `contains(&format!("{id}  {cell}"))` needle would depend on the
            // padding width and silently never match, which is how the sibling "not recorded"
            // assertion in `every_catalog_id_resolves_and_every_stored_metric_is_in_the_catalog`
            // was once VACUOUS.
            assert!(
                rendered.lines().any(|l| l.starts_with(m.id) && l.ends_with(cell.as_str())),
                "`{}` is not rendered as its unit's own `{cell}`:\n{rendered}",
                m.id
            );
        }

        // ...and the PERCENT rows are actually scaled in the real rendering, not merely in the
        // unit's unit test: `win_rate` is 0.5 on this fixture, so the cell is 50%, never 0.5%.
        assert!(
            rendered.lines().any(|l| l.starts_with("win_rate") && l.ends_with("50.0000%")),
            "a fraction reached the table unscaled:\n{rendered}"
        );
    }

    /// ⚠ **THE PUBLISHED COMPACT TABLE, pinned cell by cell.** Its value cells now come from
    /// `crate::metric_catalog::MetricUnit::render` instead of four format literals spelled in
    /// `Display` itself, and the substitution is byte-identical BY CONSTRUCTION (the unit's Percent
    /// arm is the same `format!("{:.4}%", v * 100.0)` expression). This asserts the exact strings
    /// anyway, because "byte-identical by construction" is the claim a reader most wants evidence
    /// for on the one report format that is already on people's screens and in their fixtures.
    ///
    /// The fixture's numbers are chosen by `sample_result`: equity `1000 -> 1005` is a `0.5000%`
    /// return with a `1.9802%` peak-to-trough, and one win in two trades is a `50.0000%` win rate.
    #[test]
    fn the_compact_table_cells_are_the_units_own_renderings() {
        let table = BacktestReport::from_result(None, &sample_result(), 252.0).to_string();

        for want in [
            "final_equity:  1005.00",
            "total_return:  0.5000%",
            "n_trades:      2",
            "win_rate:      50.0000%",
            "max_drawdown:  1.9802%",
        ] {
            assert!(table.contains(want), "the compact table lost `{want}`:\n{table}");
        }
        // The percent rows are SCALED — the failure this whole task is about would render
        // `0.005000%` here, and a reader would call it five thousandths of a percent.
        assert!(!table.contains("0.0050%"), "a fraction reached the table unscaled:\n{table}");
    }

    /// `Compact` delegates to `Display` rather than re-rendering, so the published human table
    /// cannot acquire a second spelling that drifts from it.
    #[test]
    fn the_compact_selection_is_the_display_table_verbatim() {
        use crate::metric_catalog::MetricSelection;
        let report = BacktestReport::from_result(Some("demo".into()), &sample_result(), 252.0);
        assert_eq!(report.render_metrics(&MetricSelection::Compact), report.to_string());
    }

    /// ⚠ **A clean run's table does not move.** Everything added here is conditional on something
    /// having happened (`is_noteworthy`) or on a stamp existing, which is what keeps every report
    /// fixture and every operator's muscle memory intact.
    #[test]
    fn a_clean_run_s_table_gains_no_row() {
        let report = BacktestReport::from_result(Some("demo".into()), &sample_result(), 252.0);
        let table = report.to_string();
        assert!(!table.contains("honesty"), "a clean run printed an honesty block:\n{table}");
        assert!(!table.contains("realism"), "an unstamped run printed a realism row:\n{table}");
    }

    /// ⚠ **THE ITEM-3 DOOR, stated as a test.** `aggregate_denials` was always a free function and
    /// the zero-trade gate was never inside it — the gate lived in
    /// `ZeroTradeReport::analyze`, its only caller. So a run that traded 400 times with 3,000
    /// orders refused by the margin gate had a reject ledger nothing ever read. Calling the same
    /// function unconditionally is the whole fix.
    #[test]
    fn the_reject_ledger_reaches_a_run_that_traded() {
        let mut r = sample_result();
        r.dropped = vec![
            ("BTCUSDT".into(), "insufficient-margin".into(), 1.0, 1.0),
            ("BTCUSDT".into(), "insufficient-margin".into(), 2.0, 1.0),
            ("ETHUSDT".into(), "volume_cap".into(), 3.0, 1.0),
        ];
        r.intrabar_both_hit = 7;

        // The run traded, so the zero-trade analyzer declines it — which is exactly the state in
        // which the counters used to vanish.
        assert!(crate::zero_trade::ZeroTradeReport::analyze(&r).is_none());

        let report = BacktestReport::from_result(None, &r, 252.0);
        let h = report.honesty.as_ref().expect("a composed report always carries the counters");
        assert_eq!(h.intrabar_both_hit, 7);
        assert_eq!(h.denials, vec![("insufficient-margin".into(), 2), ("volume_cap".into(), 1)]);
        assert!(h.is_noteworthy());

        // ...and it is VISIBLE, which is the half that was missing.
        let table = report.to_string();
        assert!(table.contains("insufficient-margin"), "{table}");
        assert!(table.contains("intrabar_both_hit"), "{table}");
    }

    /// `warmup` is recorded and deliberately does not make the block print: essentially every
    /// strategy declares one, so counting it would fire the block on every run and there would be
    /// no signal left in it.
    #[test]
    fn a_warmup_alone_is_not_noteworthy() {
        let mut r = sample_result();
        r.warmup = 200;
        let report = BacktestReport::from_result(None, &r, 252.0);
        let h = report.honesty.as_ref().unwrap();
        assert_eq!(h.warmup, 200);
        assert!(!h.is_noteworthy());
        assert!(!report.to_string().contains("honesty"));
    }

    /// The realism harm, as a test: a frictionless run and a costed one must not print the same
    /// table. Only the frictionless side gains a row.
    #[test]
    fn a_frictionless_run_says_so_and_a_costed_one_does_not() {
        let base = BacktestReport::from_result(None, &sample_result(), 252.0);

        let free = base.clone().with_realism(crate::realism::RealismStamp::new(
            [("engine.fee_rate".to_string(), "0".to_string())],
            Some("no fee_rate, no fee schedule, no slippage and no impact model".to_string()),
        ));
        assert!(free.to_string().contains("FRICTIONLESS"), "{}", free.to_string());

        let costed = base.clone().with_realism(crate::realism::RealismStamp::new(
            [("engine.fee_rate".to_string(), "0.0004".to_string())],
            None,
        ));
        assert!(!costed.to_string().contains("FRICTIONLESS"));
        assert_eq!(costed.to_string(), base.to_string(), "a costed stamp must move no row");
    }

    /// ⚠ **"Not recorded" is not "zero".** A run whose report predates the extended block must say
    /// so by name — rendering `0.0000` for its Sortino is the failure this whole `Option` exists to
    /// prevent, and it would look exactly like a strategy with no downside.
    #[test]
    fn an_unrecorded_metric_says_so_rather_than_rendering_zero() {
        use crate::metric_catalog::{MetricSelection, parse_metric_selection};

        let old = a_finite_report(); // no extended block, as an older document reads back
        assert!(old.metric_value("sortino").is_none());

        let sel = parse_metric_selection("sortino").unwrap();
        assert!(sel.needs_extended());
        let rendered = old.render_metrics(&sel);
        assert!(rendered.contains("not recorded"), "{rendered}");
        assert!(!rendered.contains("0.0000"), "a zero would read as a real answer: {rendered}");

        // The two non-metric keywords answer the same way, each about its own block.
        assert!(old.render_metrics(&MetricSelection::Honesty).contains("not recorded"));
        assert!(old.render_metrics(&MetricSelection::Realism).contains("not recorded"));
    }

    /// The long-form block, the counters and the stamp all survive the round trip — they are keys
    /// of `report.json`, which is the document a reading verb holds.
    #[test]
    fn the_three_new_blocks_round_trip_and_stay_optional() {
        let mut r = sample_result();
        r.dropped = vec![("BTCUSDT".into(), "below-min-qty".into(), 0.1, 1.0)];
        let written = BacktestReport::from_result(Some("demo".into()), &r, 252.0).with_realism(
            crate::realism::RealismStamp::new(
                [("engine.slippage".to_string(), "0.0001".to_string())],
                None,
            ),
        );

        let json = serde_json::to_string(&written).unwrap();
        let back: BacktestReport = serde_json::from_str(&json).unwrap();

        // Exact on the counts, tolerant on the float — serde_json's default (non-`float_roundtrip`)
        // parser is not always bit-exact on the way back in, the documented limitation
        // `json_round_trips` above already states.
        let (a, b) = (back.extended.as_ref().unwrap(), written.extended.as_ref().unwrap());
        assert_eq!(a.consecutive_wins, b.consecutive_wins);
        assert!(
            (a.sortino - b.sortino).abs() <= b.sortino.abs() * 1e-9 + 1e-12,
            "sortino {} vs {}",
            a.sortino,
            b.sortino
        );
        assert_eq!(back.honesty.as_ref().unwrap().denials.len(), 1);
        assert_eq!(back.realism.as_ref().unwrap().get("engine.slippage"), Some("0.0001"));

        // ...and a document that carries none of the three still parses, which is what makes them
        // safe to add to a file already on people's disks.
        let bare = oldest_report(None);
        let old: BacktestReport = serde_json::from_str(&bare).unwrap();
        assert!(old.extended.is_none() && old.honesty.is_none() && old.realism.is_none());
    }
}
