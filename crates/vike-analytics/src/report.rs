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
//! vike-model-only crate — while `harness` as a whole lives inside vike-backtest, which pulls
//! vike-data + rayon at DEFAULT features since the 2026-09-27 feature collapse (the concrete
//! DataFusion backend is still a separate opt-in, `datafusion-store`). Living here lets the
//! DataFusion-free leaf crates build the
//! SAME summary instead of re-assembling their own: [`crate::LiveTearsheet`] (vike-report's until
//! 2026-09-28) composes this struct for the fields they share, so a live tearsheet and a
//! backtest tearsheet cannot drift
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
/// `vike-backtest` harness type the Studio plane has no business constructing, and the
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
    /// rather than a number — see `ser_f64_null_when_nonfinite`. Deliberately NOT printed by
    /// `Display` — the human table's row set predates this field and stays byte-identical; the
    /// field exists for ranking objectives (`vike_backtest::search::objective`) and JSON consumers.
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
    /// `BacktestProfile`, a `vike-backtest` harness type this crate has no business naming, so
    /// the producer attaches it with [`BacktestReport::with_realism`] instead. A `None` therefore
    /// means "nobody stamped this run", which is a different answer from a frictionless one and
    /// must not be rendered as one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub realism: Option<crate::realism::RealismStamp>,
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

mod compose;
mod display;
mod extended;
mod honesty;

pub use extended::{ExtendedMetrics, OMEGA_THRESHOLD, TAIL_CONFIDENCE};
pub use honesty::HonestyCounters;

#[cfg(test)]
mod tests;
