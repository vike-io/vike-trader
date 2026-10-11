//! `ExtendedMetrics`: the long-form metric catalog and the two constants it is computed at.

use serde::{Deserialize, Serialize};

use super::{BacktestResult, metrics};

#[cfg(doc)]
use super::{BacktestReport, periods_per_year_for_interval};

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
/// convention [`crate::LiveTearsheet`] already prints at, and the two must match or the live
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
