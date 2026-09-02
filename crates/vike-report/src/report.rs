//! `LiveTearsheet` — the assembled live performance summary.
//!
//! Composes reconstructed [`Trade`]s + an equity curve into a `vike_analytics::BacktestResult`, then
//! fills every field from `vike_analytics`. This module does NO metric math of its own, and no
//! longer any metric ASSEMBLY of its own either: the fields it shares with
//! `vike_analytics::BacktestReport` are taken FROM that struct (which is feature-free — see its
//! module doc), and only the live tearsheet's own superset fields are composed here, each a direct
//! `metrics::` call. So a live tearsheet and a backtest tearsheet over the same fills are computed
//! identically, and the shared subset cannot drift apart.

use std::fmt;
use std::path::Path;

use serde::Serialize;
use vike_analytics::{metrics, BacktestReport, BacktestResult};
use vike_model::Trade;

use crate::equity::equity_curve_from_trades;
use crate::journal_read::{fills_from_journal, JournalReadError};
use crate::trades::reconstruct_trades;

/// Annualization factor for daily periods — the LEAN/tearsheet convention. Re-exported from
/// vike-analytics (NOT a second copy of `252.0`) so the live and backtest tearsheets annualize on
/// one constant. A pure fill stream has no fixed period, so the caller passes whatever cadence its
/// equity samples represent; 252 is the default.
pub use vike_analytics::DAILY_PERIODS_PER_YEAR;

/// A flat, `Serialize`-able live performance summary. Every numeric field is a `metrics::` call
/// over the reconstructed trades and/or the equity curve — see the module doc.
///
/// NB: some metrics return `f64::INFINITY` for degenerate inputs (e.g. `profit_factor` with no
/// losing trades) — the house 0.0/inf sentinel convention of `vike_analytics::metrics`. Those are
/// preserved here verbatim. `--json` stays VALID on such a run (serde_json writes a non-finite
/// float as `null`, it does not fail), but a consumer must expect `null` where it wanted a number;
/// a real trade history with both wins and losses yields finite metrics throughout.
#[derive(Debug, Clone, Serialize)]
pub struct LiveTearsheet {
    /// Free-form label for this tearsheet (e.g. a session or account id).
    pub name: Option<String>,
    pub n_trades: usize,
    pub final_equity: f64,
    /// Fractional return from the first to the last equity point (`metrics::total_return`).
    pub total_return: f64,
    /// Σ trade PnL (`metrics::net_profit`).
    pub net_profit: f64,
    pub gross_profit: f64,
    pub gross_loss: f64,
    /// Σ round-trip fees (`metrics::total_fees`).
    pub total_fees: f64,
    pub win_rate: f64,
    pub profit_factor: f64,
    pub sharpe: f64,
    pub sortino: f64,
    pub calmar: f64,
    pub max_drawdown: f64,
    pub cagr: f64,
    pub sqn: f64,
    pub avg_win: f64,
    pub avg_loss: f64,
    pub payoff_ratio: f64,
    pub expected_payoff: f64,
    pub largest_win: f64,
    pub largest_loss: f64,
    pub consecutive_wins: usize,
    pub consecutive_losses: usize,
    /// Historical 95% Value-at-Risk of per-sample returns (`metrics::value_at_risk`).
    pub value_at_risk_95: f64,
    /// Historical 95% Expected Shortfall / CVaR (`metrics::expected_shortfall`).
    pub expected_shortfall_95: f64,
}

impl LiveTearsheet {
    /// Assemble a tearsheet from reconstructed trades + an aligned equity curve.
    ///
    /// Builds a `BacktestResult` (the same shape the backtest produces) and computes every field
    /// from `metrics::`. `periods_per_year` is the Sharpe/Sortino/Calmar/CAGR annualization factor
    /// (see [`DAILY_PERIODS_PER_YEAR`]).
    pub fn from_result_parts(
        name: Option<String>,
        trades: Vec<Trade>,
        equity_curve: Vec<f64>,
        equity_ts: Vec<i64>,
        periods_per_year: f64,
    ) -> Self {
        let n_trades = trades.len();
        let final_equity = equity_curve.last().copied().unwrap_or(0.0);
        let r = BacktestResult {
            trades,
            equity_curve,
            final_equity,
            n_trades,
            equity_ts,
            ..Default::default()
        };
        Self::from_result(name, &r, periods_per_year)
    }

    /// Compute the tearsheet from a `BacktestResult` — the reuse seam. The fields shared with
    /// [`BacktestReport`] are composed by IT (the single, feature-free assembly site) and copied
    /// across; the rest are direct `metrics::` calls, the same functions `BacktestReport` uses. So
    /// live and backtest summaries are identical over identical inputs — by construction on the
    /// shared subset, rather than by two hand-kept-in-sync field lists.
    pub fn from_result(name: Option<String>, r: &BacktestResult, periods_per_year: f64) -> Self {
        let eq = &r.equity_curve;
        let tr = &r.trades;
        // The shared subset, composed once by vike-analytics. `per_symbol_pnl` is backtest-only
        // (a live fill stream has no per-symbol curve split) and is the one field dropped here.
        let base = BacktestReport::from_result(name, r, periods_per_year);
        LiveTearsheet {
            name: base.name,
            n_trades: base.n_trades,
            final_equity: base.final_equity,
            total_return: base.total_return,
            win_rate: base.win_rate,
            sharpe: base.sharpe,
            max_drawdown: base.max_drawdown,
            // profit_factor joined the shared subset when BacktestReport grew the field (for the
            // sweep-ranking objectives) — same `metrics::profit_factor(tr)` value, now composed
            // by the single assembly site per the F31 invariant.
            profit_factor: base.profit_factor,
            // The live tearsheet's own superset — not part of the compact backtest report.
            net_profit: metrics::net_profit(tr),
            gross_profit: metrics::gross_profit(tr),
            gross_loss: metrics::gross_loss(tr),
            total_fees: metrics::total_fees(tr),
            sortino: metrics::sortino(eq, periods_per_year),
            calmar: metrics::calmar(eq, periods_per_year),
            cagr: metrics::cagr(eq, periods_per_year),
            sqn: metrics::sqn(tr),
            avg_win: metrics::avg_win(tr),
            avg_loss: metrics::avg_loss(tr),
            payoff_ratio: metrics::payoff_ratio(tr),
            expected_payoff: metrics::expected_payoff(tr),
            largest_win: metrics::largest_win(tr),
            largest_loss: metrics::largest_loss(tr),
            consecutive_wins: metrics::consecutive_wins(tr),
            consecutive_losses: metrics::consecutive_losses(tr),
            value_at_risk_95: metrics::value_at_risk(eq, 0.95),
            expected_shortfall_95: metrics::expected_shortfall(eq, 0.95),
        }
    }

    /// Read a vike-core command journal directory, reconstruct trades from its fill stream, build
    /// the realized-only fallback equity curve from `seed`, and assemble the tearsheet.
    ///
    /// This is the primary live-tearsheet entry point. The equity curve is realized-only (derived
    /// from trade PnLs); when a stored `kind=equity` series exists, prefer
    /// [`Self::from_result_parts`] with [`crate::equity::equity_curve_from_store`] for a true
    /// mark-to-market curve.
    pub fn from_journal(
        dir: &Path,
        seed: f64,
        periods_per_year: f64,
    ) -> Result<Self, JournalReadError> {
        let fills = fills_from_journal(dir)?;
        let trades = reconstruct_trades(&fills);
        let (equity, ts) = equity_curve_from_trades(seed, &trades);
        Ok(Self::from_result_parts(None, trades, equity, ts, periods_per_year))
    }
}

impl fmt::Display for LiveTearsheet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "=== Live Tearsheet: {} ===", self.name.as_deref().unwrap_or("(unnamed)"))?;
        writeln!(f, "trades:              {}", self.n_trades)?;
        writeln!(f, "final_equity:        {:.2}", self.final_equity)?;
        writeln!(f, "total_return:        {:.4}%", self.total_return * 100.0)?;
        writeln!(f, "net_profit:          {:.2}", self.net_profit)?;
        writeln!(f, "gross_profit:        {:.2}", self.gross_profit)?;
        writeln!(f, "gross_loss:          {:.2}", self.gross_loss)?;
        writeln!(f, "total_fees:          {:.4}", self.total_fees)?;
        writeln!(f, "win_rate:            {:.4}%", self.win_rate * 100.0)?;
        writeln!(f, "profit_factor:       {:.4}", self.profit_factor)?;
        writeln!(f, "sharpe:              {:.4}", self.sharpe)?;
        writeln!(f, "sortino:             {:.4}", self.sortino)?;
        writeln!(f, "calmar:              {:.4}", self.calmar)?;
        writeln!(f, "max_drawdown:        {:.4}%", self.max_drawdown * 100.0)?;
        writeln!(f, "cagr:                {:.4}%", self.cagr * 100.0)?;
        writeln!(f, "sqn:                 {:.4}", self.sqn)?;
        writeln!(f, "avg_win:             {:.2}", self.avg_win)?;
        writeln!(f, "avg_loss:            {:.2}", self.avg_loss)?;
        writeln!(f, "payoff_ratio:        {:.4}", self.payoff_ratio)?;
        writeln!(f, "expected_payoff:     {:.4}", self.expected_payoff)?;
        writeln!(f, "largest_win:         {:.2}", self.largest_win)?;
        writeln!(f, "largest_loss:        {:.2}", self.largest_loss)?;
        writeln!(f, "consecutive_wins:    {}", self.consecutive_wins)?;
        writeln!(f, "consecutive_losses:  {}", self.consecutive_losses)?;
        writeln!(f, "value_at_risk_95:    {:.4}", self.value_at_risk_95)?;
        writeln!(f, "expected_shortfall:  {:.4}", self.expected_shortfall_95)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trade(pnl: f64, fees: f64, is_long: bool, exit_ts: i64) -> Trade {
        Trade {
            entry_price: 100.0,
            exit_price: 100.0 + pnl,
            size: 1.0,
            pnl,
            fees,
            entry_ts: exit_ts - 1,
            exit_ts,
            symbol: "BTCUSDT".to_string(),
            mae: 0.0,
            mfe: 0.0,
            is_long,
        }
    }

    /// A small trades vec with BOTH wins and losses so every metric is finite.
    fn sample_trades() -> Vec<Trade> {
        vec![
            trade(10.0, 0.5, true, 2_000),
            trade(-4.0, 0.5, true, 3_000),
            trade(6.0, 0.3, false, 4_000),
            trade(-2.0, 0.2, true, 5_000),
        ]
    }

    #[test]
    fn every_stat_is_finite_and_matches_direct_metrics() {
        let trades = sample_trades();
        let (eq, ts) = equity_curve_from_trades(1_000.0, &trades);
        let sheet = LiveTearsheet::from_result_parts(
            Some("sess-1".into()),
            trades.clone(),
            eq.clone(),
            ts,
            DAILY_PERIODS_PER_YEAR,
        );

        // finiteness of every float field
        for (label, v) in [
            ("total_return", sheet.total_return),
            ("net_profit", sheet.net_profit),
            ("gross_profit", sheet.gross_profit),
            ("gross_loss", sheet.gross_loss),
            ("total_fees", sheet.total_fees),
            ("win_rate", sheet.win_rate),
            ("profit_factor", sheet.profit_factor),
            ("sharpe", sheet.sharpe),
            ("sortino", sheet.sortino),
            ("calmar", sheet.calmar),
            ("max_drawdown", sheet.max_drawdown),
            ("cagr", sheet.cagr),
            ("sqn", sheet.sqn),
            ("avg_win", sheet.avg_win),
            ("avg_loss", sheet.avg_loss),
            ("payoff_ratio", sheet.payoff_ratio),
            ("expected_payoff", sheet.expected_payoff),
            ("largest_win", sheet.largest_win),
            ("largest_loss", sheet.largest_loss),
            ("value_at_risk_95", sheet.value_at_risk_95),
            ("expected_shortfall_95", sheet.expected_shortfall_95),
        ] {
            assert!(v.is_finite(), "{label} must be finite, got {v}");
        }

        // wiring: fields equal the direct metrics calls (no reimplementation).
        assert_eq!(sheet.n_trades, 4);
        assert_eq!(sheet.net_profit, metrics::net_profit(&trades));
        assert_eq!(sheet.win_rate, metrics::win_rate(&trades));
        assert_eq!(sheet.profit_factor, metrics::profit_factor(&trades));
        assert_eq!(sheet.sharpe, metrics::sharpe(&eq, DAILY_PERIODS_PER_YEAR));
        assert_eq!(sheet.max_drawdown, metrics::max_drawdown(&eq));
        // net profit sanity: 10 - 4 + 6 - 2 = 10
        assert_eq!(sheet.net_profit, 10.0);
        // final equity = seed + Σ(pnl - fees) = 1000 + (10-.5)+(-4-.5)+(6-.3)+(-2-.2) = 1008.5
        assert!((sheet.final_equity - 1_008.5).abs() < 1e-9);
    }

    /// The F31 invariant: every field `BacktestReport` also composes is taken FROM it, so the
    /// live tearsheet and the backtest report cannot drift apart on their shared subset.
    #[test]
    fn shared_fields_equal_the_backtest_report_composition() {
        let trades = sample_trades();
        let (eq, ts) = equity_curve_from_trades(1_000.0, &trades);
        let sheet = LiveTearsheet::from_result_parts(
            Some("sess-1".into()),
            trades.clone(),
            eq.clone(),
            ts.clone(),
            DAILY_PERIODS_PER_YEAR,
        );
        let r = BacktestResult {
            trades,
            equity_curve: eq,
            final_equity: sheet.final_equity,
            n_trades: sheet.n_trades,
            equity_ts: ts,
            ..Default::default()
        };
        let report = BacktestReport::from_result(Some("sess-1".into()), &r, DAILY_PERIODS_PER_YEAR);

        assert_eq!(sheet.name, report.name);
        assert_eq!(sheet.n_trades, report.n_trades);
        assert_eq!(sheet.final_equity, report.final_equity);
        assert_eq!(sheet.total_return, report.total_return);
        assert_eq!(sheet.win_rate, report.win_rate);
        assert_eq!(sheet.sharpe, report.sharpe);
        assert_eq!(sheet.max_drawdown, report.max_drawdown);
        assert_eq!(sheet.profit_factor, report.profit_factor);
    }

    #[test]
    fn display_is_non_empty_and_labeled() {
        let trades = sample_trades();
        let (eq, ts) = equity_curve_from_trades(1_000.0, &trades);
        let sheet = LiveTearsheet::from_result_parts(None, trades, eq, ts, DAILY_PERIODS_PER_YEAR);
        let s = sheet.to_string();
        assert!(!s.is_empty());
        assert!(s.contains("Live Tearsheet"));
        assert!(s.contains("(unnamed)"));
        assert!(s.contains("sharpe:"));
        assert!(s.contains("win_rate:"));
        assert!(s.contains("max_drawdown:"));
    }

    #[test]
    fn serde_round_trips() {
        let trades = sample_trades();
        let (eq, ts) = equity_curve_from_trades(1_000.0, &trades);
        let sheet = LiveTearsheet::from_result_parts(
            Some("acct-42".into()),
            trades,
            eq,
            ts,
            DAILY_PERIODS_PER_YEAR,
        );
        let json = serde_json::to_string(&sheet).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["name"], "acct-42");
        assert_eq!(parsed["n_trades"], 4);
        // a float field survives the round-trip within serde_json's parse tolerance
        let got = parsed["net_profit"].as_f64().unwrap();
        assert!((got - sheet.net_profit).abs() <= sheet.net_profit.abs() * 1e-9 + 1e-12);
    }
}
