//! `TradingResults` — the printable/serializable metrics summary: composes the EXISTING
//! `crate::metrics` functions over a [`TradingRecord`] into one flat, `Serialize` struct.
//!
//! This module does no metric MATH of its own — every number is a direct pass-through or a
//! single `metrics::` call, so a divergence here is a wiring bug, not a math bug (see the
//! `matches_direct_metrics_calls` test, which asserts field-for-field equality against calling
//! `metrics::` directly on the same inputs).
//!
//! FEATURE-FREE by design (it is `crate::results`, not `vike_backtest::run::report`). The
//! composition needs nothing but `serde` + `crate::metrics`, which is exactly why it lives in this
//! vike-model-only crate — while `harness` as a whole lives inside vike-backtest, which pulls
//! vike-data + rayon at DEFAULT features since the 2026-09-27 feature collapse (the concrete
//! DataFusion backend reaches it as a dev dependency only). Living here lets the
//! DataFusion-free leaf crates build the
//! SAME summary instead of re-assembling their own: [`crate::TradingResultsTable`] (vike-report's until
//! 2026-09-28) composes this struct for the fields they share, so a live tearsheet and a
//! backtest tearsheet cannot drift
//! apart. `vike_backtest::run::report` re-exports everything below (so the `backtesthub` bin's
//! paths are unchanged) and adds only the pieces that genuinely need the gated profile parser:
//! `periods_per_year(&ProfileBacktest)` and `realism_stamp(&ProfileBacktest)`.
//!
//! ⚠ **That "fields they share" used to be EIGHT of twenty-six, and the gap was the defect.** The
//! live tearsheet composed Sortino, Calmar, CAGR, SQN, VaR and expected shortfall with its own
//! `metrics::` calls and its own `0.95`, while a backtest report carried none of them — so the
//! shared subset could not drift and everything outside it was free to. [`TradingResultsExtendedMetrics`] is the
//! one composition of those thirty numbers and both doors take them from it; the shared subset is
//! now the whole of both.

use serde::{Deserialize, Serialize};

use crate::metrics;
use crate::record::TradingRecord;

/// Annualization factor for daily (`1d`) bars — the LEAN/tearsheet convention.
pub const DAILY_PERIODS_PER_YEAR: f64 = 252.0;
/// Fallback for non-daily / tick runs (a tick stream has no fixed period) — kept equal to
/// [`DAILY_PERIODS_PER_YEAR`] today so the Sharpe scale is at least consistent.
pub const DEFAULT_PERIODS_PER_YEAR: f64 = 252.0;

/// The starting equity an account's report folds its curve from when the caller names none: the one default
/// of the live tearsheet's `seed`, `vike-cli report`'s `--cash` and the account window's history view. ONE
/// number because `total_return`, `cagr` and `sharpe` all scale with the equity base: two doors on two
/// defaults would publish two answers for one account, each internally consistent.
pub const DEFAULT_REPORT_SEED: f64 = 10_000.0;

/// The annualization factor for a bar series of `interval`: the number of RETURN OBSERVATIONS a
/// year produces at that bar step, which is exactly what [`metrics::sharpe`]'s
/// `sqrt(periods_per_year)` needs.
///
/// # THE ONE HOME for this derivation, and why it moved here
///
/// It lives beside the constants it scales because TWO planes need it and only one ever had it.
/// `vike_backtest::run::report::periods_per_year` is now a profile-shaped wrapper over this
/// function; `vike-studio-core`'s slice-shaped callers pass their `DataSlice::interval` straight
/// in. Before this function existed the Studio plane passed a bare `252.0`, so the SAME strategy
/// over the SAME 1h bars reported a walk-forward Sharpe differing by `sqrt(24) ≈ 4.9x` between the
/// CLI/MCP door and the Studio door — while a doc comment on the harness side asserted the two
/// could not disagree, and no test compared them.
///
/// Keyed on the interval STRING rather than on a profile: a `ProfileBacktest` is a
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
/// * An interval with no bar width ([`vike_marketdata::interval::bar_step_ms`]) falls back
///   to [`DEFAULT_PERIODS_PER_YEAR`] rather than fabricating a scale — this is a reporting knob,
///   not a validation site, and each caller's own parser is what rejects a malformed interval.
///
/// A caller with no fixed period AT ALL — a tick stream — asks [`periods_per_year_for`] with
/// `None`, which answers [`DEFAULT_PERIODS_PER_YEAR`]: there is no honest observation count to
/// derive.
pub fn periods_per_year_for_interval(interval: &str) -> f64 {
    vike_marketdata::interval::bar_step_ms(interval)
        .map_or(DEFAULT_PERIODS_PER_YEAR, periods_per_year_for_step_ms)
}

/// [`periods_per_year_for_interval`]'s scale for a step of `step_ms` milliseconds, `252 · (86_400_000 / step_ms)`: the
/// one formula, which a TICK run's curve kept by time is annualized by too
/// ([`crate::record::TradingRecord::periods_per_year`]), so its Sharpe is on a bar run's scale at the same step.
pub fn periods_per_year_for_step_ms(step_ms: i64) -> f64 {
    // A day in the unit `vike_marketdata::interval::interval_ms` counts in, from the one home of time units.
    DAILY_PERIODS_PER_YEAR * (vike_model::MS_PER_DAY_F64 / step_ms as f64)
}

/// The annualization factor of a run over BARS of `bar_interval` — or, for `None` (a tick stream,
/// or no slice at all), [`DEFAULT_PERIODS_PER_YEAR`]: a stream with no fixed period has no honest
/// observation count. **The one tick-or-bar rule**: `vike_backtest::run::report::periods_per_year`
/// (a profile) and the Studio's `display_periods_per_year` (a picked slice) both answer through it,
/// so the two doors cannot pick different fallbacks for the same run. A tick run whose curve is kept
/// by time (the default since 2026-11-20) is annualized at its own step instead
/// ([`crate::record::TradingRecord::periods_per_year`], read by [`TradingResults::from_record`]).
pub fn periods_per_year_for(bar_interval: Option<&str>) -> f64 {
    bar_interval.map_or(DEFAULT_PERIODS_PER_YEAR, periods_per_year_for_interval)
}

/// The annualization factor for a curve sampled once per UTC CALENDAR day — the live tearsheet's
/// (`crate::trading_results_table_from_fills`): 365 when every market in it trades every day (a 24/7 book),
/// [`DAILY_PERIODS_PER_YEAR`] (the 252 trading days of a session market) otherwise.
///
/// The one home of that choice, beside [`periods_per_year_for_interval`], which keeps 252 for
/// every bar series a backtest reports. Whether a market trades every day is the caller's to say
/// (`vike_catalog::trades_every_day`): this crate names no venue.
pub fn periods_per_year_for_days(every_day: bool) -> f64 {
    if every_day { 365.0 } else { DAILY_PERIODS_PER_YEAR }
}

/// A flat, `Serialize`-able summary of one backtest run. Every field is either copied straight
/// from the [`TradingRecord`] or computed by an existing `crate::metrics` function — see the
/// module doc.
///
/// The SCALARS are deliberately compact — they are the `backtesthub` bin's human table, the sweep's
/// ranking source and every reading verb's column set, and none of those wants an
/// everything-drawer.
///
/// ⚠ **The long-form catalog is no longer somebody else's problem, and this paragraph used to say
/// it was.** It read "a caller needing the long-form stat catalog (Sortino/Calmar/CAGR/SQN/VaR/…)
/// composes this for the shared fields and adds its own from `metrics::` — `vike_report::
/// Tearsheet` is the worked example", and what that arrangement produced was measurable: the
/// LIVE door printed twenty-six numbers over a [`TradingRecord`] and the BACKTEST door printed
/// eight over the same one, so the same fills answered a richer question depending on which verb
/// was asked. Those thirty numbers are [`Self::extended`] now, composed by one site
/// ([`TradingResultsExtendedMetrics`]) that both doors take them from — and stored, because
/// `vike_runs::RunSeries` decimates the equity curve and a reader recomputing a VaR from
/// what is on disk would get a statistic of a thinned curve. [`crate::metric_catalog`] is the
/// roster over both halves.
///
/// # ⚠ EIGHT fields carry `#[serde(default)]`, and the other seven do not
///
/// This type is the content of `report.json` inside `<project>/user_data/runs/<run_id>/` — a
/// document written to people's disks since before it could be read back at all. Every field whose
/// own doc records it as added after the first version shipped ([`Self::profit_factor`],
/// [`Self::funding_paid`], [`Self::per_symbol_pnl`], [`Self::zero_trade`], [`Self::extended`],
/// [`Self::honesty`], [`Self::realism`], [`Self::data_warnings`]) MUST default, because a
/// required field makes every report written before it existed fail to parse — the failure
/// `vike_runs::RunManifest::schema` carries the same defence against, where a parse
/// failure becomes a DROPPED ROW in `crates/vike-study-runner/src/listing.rs`'s `list_runs`.
///
/// ⚠ For the three `Option` blocks that defaulting has a SECOND consequence, and it is the one to
/// carry: `None` means THIS RUN DID NOT RECORD IT, which is not the same answer as a zero. A reader
/// that renders `0.0` for an unrecorded Sortino has published "this strategy had no downside", and
/// `crate::metric_catalog::MetricSelection::needs_extended` exists so it can refuse instead.
///
/// ⚠ **The other seven are REQUIRED, deliberately, and defaulting them was a real defect.** They
/// have existed since the document's first version, so no `report.json` on anyone's disk can lack
/// one — defaulting buys nothing and removes the only structural check that the bytes are a report
/// AT ALL. With every field optional, `serde_json::from_str::<TradingResults>("{}")` SUCCEEDS and
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
///
/// `PartialEq` for the walk-forward wire, whose steps and overall results are these
/// (`crate::walkforward::WalkforwardResultsStep`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TradingResults {
    /// The profile's free-form `name`, if it had one (`ProfileBacktest::name`).
    pub name: Option<String>,
    pub final_equity: f64,
    /// Fractional return from the first to the last equity-curve point (`metrics::total_return`).
    pub total_return: f64,
    pub n_trades: usize,
    /// Fraction of trades with positive PnL (`metrics::win_rate`).
    pub win_rate: f64,
    /// Annualized Sharpe of per-bar returns, or of a tick run's per-step closes (`metrics::sharpe`,
    /// [`Self::returns_interval_ms`]).
    pub sharpe: f64,
    /// Largest peak-to-trough drop as a positive fraction of the peak (`metrics::max_drawdown`).
    pub max_drawdown: f64,
    /// Gross profit / gross loss (`metrics::profit_factor`). House sentinel: `f64::INFINITY` when
    /// there are no losing trades but some profit, `0.0` when there is neither. Serialized as
    /// `null` when non-finite, so a `--json` consumer sees an explicit "no meaningful ratio"
    /// rather than a number — see `vike_trade_ledger::serialize_f64_null_when_nonfinite`. Deliberately NOT printed by
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
        serialize_with = "vike_trade_ledger::serialize_f64_null_when_nonfinite",
        deserialize_with = "vike_trade_ledger::de_f64_null_as_infinity"
    )]
    pub profit_factor: f64,
    /// NET perp funding cashflow over the run (received-positive / paid-negative) — the twin of
    /// `vike_execution::Account.funding_paid`, straight from [`TradingRecord::funding_paid`]. `0.0` for a
    /// non-perp / no-funding run. Printed by `Display` ONLY when nonzero (so a spot backtest's table
    /// stays byte-identical to before this field existed); always present in `--json`.
    #[serde(default)]
    pub funding_paid: f64,
    /// Multi-symbol event runs only; empty for single-symbol/vector runs (see
    /// [`TradingRecord::per_symbol_pnl`]).
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
    /// The run is INVALID: its equity reached zero ([`TradingRecord::equity_reached_zero`]'s words, which
    /// name the day). `Some` only for such a run, so every other report is byte-identical (skipped when
    /// `None`). When present, `Display` prints the words and leaves out every return, ratio and
    /// probability, and [`Self::rendered_rows`] (the tearsheet's, so the HTML too) keeps only the money
    /// and count rows after them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalid: Option<String>,
    /// The LONG-FORM metric catalog — Sortino, Calmar, CAGR, SQN, VaR, expected shortfall, return
    /// skew and the rest of [`crate::metric_catalog::METRICS`]. See [`TradingResultsExtendedMetrics`] for why it
    /// is computed at RUN time and stored rather than derived by a reader.
    ///
    /// `None` for a `report.json` written before this block existed, and for a report built by a
    /// literal rather than by [`TradingResults::from_record`]. A reader must refuse a selection
    /// that needs it rather than rendering zeros — the distinction
    /// [`crate::metric_catalog::MetricSelection::needs_extended`] exists to make askable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extended: Option<TradingResultsExtendedMetrics>,
    /// What the run DEFERRED, skipped, could not price and refused — see [`TradingResultsHonesty`].
    ///
    /// `None` on the same terms as [`Self::extended`]. Printed by `Display` only when something is
    /// non-zero, so a clean run's table stays byte-identical to before this field existed (the same
    /// discipline [`Self::funding_paid`] follows).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub honesty: Option<TradingResultsHonesty>,
    /// The cost model the run actually ran under — see [`crate::realism::RealismStamp`].
    ///
    /// ⚠ **`from_record` leaves this `None` and that is deliberate**: the stamp is resolved from a
    /// `ProfileBacktest`, a `vike-backtest` harness type this crate has no business naming, so
    /// the producer attaches it with [`TradingResults::with_realism`] instead. A `None` therefore
    /// means "nobody stamped this run", which is a different answer from "no fees" and must not be
    /// rendered as one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub realism: Option<crate::realism::RealismStamp>,
    /// What the run found wrong with its data and ran anyway ([`TradingRecord::data_warnings`]):
    /// on `report.json` and on the wire, which is how an `--addr` caller learns of them at all.
    /// Skipped when empty and printed by `Display` only when not, so a clean report is byte-identical
    /// to one written before this field existed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data_warnings: Vec<String>,
    /// A COIN LEDGER's money in dollars ([`TradingRecord::settle_price`], decision 0258): every money
    /// field above is then an amount of the contract's settle coin (the realism stamp's
    /// `engine.account_currency` names it), and this is the final equity valued at the contract's last
    /// price. `None`, skipped and unprinted, for every other run, so its report is byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_dollars: Option<TradingResultsInDollars>,
    /// Closed-trade accounting ([`vike_trade_ledger`]): the trades-analysis statistics of the run's fills booked
    /// one lot per entry, first in first out, every row net of both commissions, in all, long and short columns
    /// ([`TradingRecord::closed_trades`]). `None`, skipped and unprinted, unless the run armed it
    /// (`engine.closed_trades`, which the realism stamp records like every engine key); no
    /// existing field reads it, so no existing number moves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_trades: Option<vike_trade_ledger::TradeClosedMetrics>,
    /// The strategy file the run ran: its path under the project's `strategies/` folder (the
    /// profile's `[strategy] file`). Attached by the producer ([`Self::with_strategy`]). Empty and
    /// skipped where none was recorded (a report written before the field), so such a report is
    /// byte-identical. `Display` does not print it: the table's rows predate it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub strategy_file: String,
    /// The sha256 of [`Self::strategy_file`]'s text as the run read it: two runs with the same hash
    /// ran the same code. Empty and skipped where no file was read; not printed by `Display`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub strategy_sha256: String,
    /// The numbered SDK [`Self::strategy_file`]'s artifact was built against (decision 0296: a Rust
    /// file pins `[strategy] sdk = N` or gets the newest SDK on the box). With the sha256 and the
    /// host's build it says which code ran. `None`, skipped and unprinted, for a `.rhai` file, a build
    /// against a development tree (no numbered SDK) and a report written before the field, so such a
    /// report keeps its bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy_sdk: Option<u32>,
    /// The step, in ms, a TICK run's return statistics were sampled at
    /// ([`crate::record::TradingRecordSampling::interval_ms`], decision 0078's amendment of 2026-11-20): its Sharpe,
    /// Sortino, volatility, tails, PSR and the like read one equity close a step, annualized at that step, as a bar
    /// run's read one a bar; its maximum drawdown is still every event's. `None`, skipped and unprinted, for a bar run
    /// and for a tick run that kept every event (`engine.equity_sample_every = 1`). ⚠ A tick report without it (every
    /// one saved before 2026-11-20) has per-event return statistics, which are not comparable with these.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub returns_interval_ms: Option<i64>,
    /// A PORTFOLIO backtest's table of its mounts ([`TradingRecord::mounts`], copied): each strategy file's own
    /// trades, win %, profit factor and PnL over one account. `None`, skipped and unprinted, for every other run, so
    /// its report is byte-identical. The account's figures above are every mount's together; a mount has no curve or
    /// Sharpe of its own, because no equity is any one mount's (decision 0048).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mounts: Option<TradingResultsMounts>,
    /// What the strategy's hooks FAILED with over the run ([`TradingRecord::strategy_failures`], copied): how many,
    /// when first, the first distinct messages, and `disabled`, the typed verdict that the strategy stopped
    /// running its hooks. Skipped when nothing failed, so a clean report is byte-identical; when present,
    /// `Display` prints its one loud line above every number.
    #[serde(default, skip_serializing_if = "vike_model::StrategyFailures::is_empty")]
    pub strategy_failures: vike_model::StrategyFailures,
    /// Who produced these results ([`TradingRecord::source`]). NOT serialized, so `report.json` and the
    /// wire keep their bytes: [`Self::from_record`] carries the record's, and a stored report gets its
    /// source from the one typed read, [`Self::read_stored`]. `None` for a report a bare `serde_json`
    /// parse or a literal built.
    #[serde(skip)]
    pub source: Option<crate::record::TradingRecordSource>,
    /// The span these results cover ([`TradingRecord::period`]), on the same terms as [`Self::source`].
    #[serde(skip)]
    pub period: Option<crate::record::TradingPeriod>,
}

/// A coin ledger's final equity in dollars, at the price it was valued at ([`TradingResults::in_dollars`]).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TradingResultsInDollars {
    /// The contract's last price: the dollars one settle coin is worth.
    pub price: f64,
    /// [`TradingResults::final_equity`] times [`Self::price`].
    pub final_equity: f64,
}

/// A portfolio's mounts in its report ([`TradingResults::mounts`]): one row a mount, in mount order, and what no
/// mount's attribution holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TradingResultsMounts {
    pub rows: Vec<TradingResultsMount>,
    /// The account's PnL (final equity less the starting cash) less every mount's: what reached no mount, as funding,
    /// a liquidation or a settlement does (decision 0116: a fill no mount placed is no mount's).
    pub unattributed_pnl: f64,
}

/// One mount of a portfolio backtest: its strategy file, the slots it trades and what its own fills made. Its trades,
/// win % and profit factor are its closed-trade ledger's, net of commission; its PnL is its attributed fills' realized
/// and open PnL at the run's last bar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TradingResultsMount {
    /// Its place in the profile's `[[mounts]]`, from 0: the order it was dispatched in.
    pub mount: usize,
    /// Its strategy file, as the profile names it.
    pub strategy_file: String,
    /// The sha256 of the file's text as the run read it; empty and skipped where no door read it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub strategy_sha256: String,
    /// The SDK its artifact was built against, as [`TradingResults::strategy_sdk`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy_sdk: Option<u32>,
    /// The slots it trades, its own first.
    pub symbols: Vec<String>,
    pub n_trades: usize,
    /// The fraction of its closed trades with a positive net.
    pub win_rate: f64,
    /// Gross profit over gross loss of its closed trades, the house `inf` when nothing lost and something won
    /// (`null` on the wire, as [`TradingResults::profit_factor`]).
    #[serde(
        default,
        serialize_with = "vike_trade_ledger::serialize_f64_null_when_nonfinite",
        deserialize_with = "vike_trade_ledger::de_f64_null_as_infinity"
    )]
    pub profit_factor: f64,
    /// Its realized and open PnL at the run's last bar, net of commission.
    pub pnl: f64,
    /// The leverage cap it ran under (its own, else `engine.max_mount_leverage`); `None` uncapped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_leverage: Option<f64>,
    /// Its loss budget (decision 0249); `None` without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_loss: Option<f64>,
    /// How many of its opening orders its leverage cap refused (`over-mount-leverage`).
    pub leverage_refusals: usize,
    /// When its loss budget latched it (epoch ms), after which it placed nothing; `None` when it never did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latched_at: Option<i64>,
}

mod compose;
mod display;
mod extended;
mod honesty;

pub use extended::{OMEGA_THRESHOLD, TAIL_CONFIDENCE, TradingResultsExtendedMetrics};
pub use honesty::TradingResultsHonesty;

#[cfg(test)]
mod tests;
