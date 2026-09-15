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
//! paths are unchanged) and adds only the one piece that genuinely needs the gated profile parser:
//! `periods_per_year(&BacktestProfile)`.

use serde::{Deserialize, Serialize};
use std::fmt;

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
/// Deliberately COMPACT: this is the `backtest` bin's human table / `--json` schema and the
/// sweep's ranking source, not an everything-drawer. A caller needing the long-form stat catalog
/// (Sortino/Calmar/CAGR/SQN/VaR/…) composes this for the shared fields and adds its own from
/// `metrics::` — `vike_report::LiveTearsheet` is the worked example.
///
/// # ⚠ Exactly the FOUR fields added later carry `#[serde(default)]`, and no others
///
/// This type is the content of `report.json` inside `<project>/user_data/runs/<run_id>/` — a
/// document written to people's disks since before it could be read back at all. The four whose
/// own field docs record them as added after the first version shipped ([`Self::profit_factor`],
/// [`Self::funding_paid`], [`Self::per_symbol_pnl`], [`Self::zero_trade`]) MUST default, because a
/// required field makes every report written before it existed fail to parse — the failure
/// `vike_model::runs::RunManifest::schema` carries the same defence against, where a parse
/// failure becomes a DROPPED ROW in `crates/vike-studio-core/src/listing.rs`'s `list_runs`.
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
        }
    }
}

impl fmt::Display for BacktestReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "name:          {}", self.name.as_deref().unwrap_or("(unnamed)"))?;
        // A zero-trade / flat-equity run prints its DIAGNOSIS instead of the bare all-zero metrics
        // table. `zero_trade` is `Some` only for such a run (see `ZeroTradeReport::analyze`), so a
        // run with trades falls straight through to the unchanged table below and is byte-identical.
        if let Some(zt) = &self.zero_trade {
            writeln!(f, "final_equity:  {:.2}", self.final_equity)?;
            writeln!(f, "n_trades:      0")?;
            write!(f, "{zt}")?;
            return Ok(());
        }
        writeln!(f, "final_equity:  {:.2}", self.final_equity)?;
        writeln!(f, "total_return:  {:.4}%", self.total_return * 100.0)?;
        writeln!(f, "n_trades:      {}", self.n_trades)?;
        writeln!(f, "win_rate:      {:.4}%", self.win_rate * 100.0)?;
        writeln!(f, "sharpe:        {:.4}", self.sharpe)?;
        writeln!(f, "max_drawdown:  {:.4}%", self.max_drawdown * 100.0)?;
        // Funding P&L: shown ONLY when nonzero, so a spot / no-funding report is byte-identical to
        // before this field existed (same discipline as the omitted profit_factor row).
        if self.funding_paid != 0.0 {
            writeln!(f, "funding_paid:  {:.2}", self.funding_paid)?;
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
}
