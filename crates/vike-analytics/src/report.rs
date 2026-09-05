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

use serde::Serialize;
use std::fmt;

use crate::metrics;
use crate::result::BacktestResult;

/// Annualization factor for daily (`1d`) bars — the LEAN/tearsheet convention.
pub const DAILY_PERIODS_PER_YEAR: f64 = 252.0;
/// Fallback for non-daily / tick runs (a tick stream has no fixed period) — kept equal to
/// [`DAILY_PERIODS_PER_YEAR`] today so the Sharpe scale is at least consistent.
pub const DEFAULT_PERIODS_PER_YEAR: f64 = 252.0;

/// A flat, `Serialize`-able summary of one backtest run. Every field is either copied straight
/// from the [`BacktestResult`] or computed by an existing `crate::metrics` function — see the
/// module doc.
///
/// Deliberately COMPACT: this is the `backtest` bin's human table / `--json` schema and the
/// sweep's ranking source, not an everything-drawer. A caller needing the long-form stat catalog
/// (Sortino/Calmar/CAGR/SQN/VaR/…) composes this for the shared fields and adds its own from
/// `metrics::` — `vike_report::LiveTearsheet` is the worked example.
#[derive(Debug, Clone, Serialize)]
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
    #[serde(serialize_with = "ser_f64_null_when_nonfinite")]
    pub profit_factor: f64,
    /// NET perp funding cashflow over the run (received-positive / paid-negative) — the twin of
    /// `vike_exec::Account.funding_paid`, straight from [`BacktestResult::funding_paid`]. `0.0` for a
    /// non-perp / no-funding run. Printed by `Display` ONLY when nonzero (so a spot backtest's table
    /// stays byte-identical to before this field existed); always present in `--json`.
    pub funding_paid: f64,
    /// Multi-symbol event runs only; empty for single-symbol/vector runs (see
    /// [`BacktestResult::per_symbol_pnl`]).
    pub per_symbol_pnl: Vec<(String, f64)>,
    /// Diagnosis of a zero-trade / flat-equity run — a RANKED list of probable causes composed by
    /// [`crate::zero_trade::ZeroTradeReport::analyze`] from the diagnostic counters the run already
    /// carried. `Some` ONLY when the run closed no trades AND its equity never moved (see
    /// `analyze`), so it is `None` — skipped on serialization (like the sibling `score` field) and
    /// absent from `Display` — for any run with trades, keeping a normal report byte-identical to
    /// before this field existed. When present, `Display` prints the diagnosis INSTEAD OF the
    /// bare all-zero metrics table.
    #[serde(skip_serializing_if = "Option::is_none")]
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
}
