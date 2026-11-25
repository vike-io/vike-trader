//! `TradingResultsExtendedMetrics`: the long-form metric catalog and the two constants it is computed at.

use serde::{Deserialize, Serialize};

use super::{TradingRecord, metrics};

#[cfg(doc)]
use super::{TradingResults, periods_per_year_for_interval};

/// The long-form metric catalog for one run: every [`crate::metric_catalog::METRICS`] id whose
/// `home` is [`crate::metric_catalog::MetricHome::Extended`], each one a single `crate::metrics`
/// call over the same [`TradingRecord`] the compact scalars were composed from.
///
/// # Why this is STORED rather than derived by a reader
///
/// A reader holding a run record could recompute most of these — and would get different numbers.
/// `vike_runs::RunSeries` DECIMATES the equity curve once a run exceeds
/// `vike_runs::MAX_EQUITY_SAMPLES`, so a Sortino or a VaR recomputed from what is on disk is
/// a statistic of a thinned curve, shallower and smoother than the one the run actually produced.
/// That is exactly the argument `crates/vike-cli/src/cmd/backtest/read/show.rs` already makes when it
/// refuses `--breakdown`. Computing here, once, from the whole curve, is the only spelling in which
/// the stored number and the run's own Sharpe come from the same samples.
///
/// # Nothing here is new math
///
/// Every field is one `crate::metrics` call, exactly as [`TradingResults`]'s own fields are — so a
/// divergence in this struct is a wiring bug, not a math bug, and
/// `extended_matches_direct_metrics_calls` asserts it field-for-field.
///
/// # Four fields answer `inf`, and read back as it
///
/// `calmar`, `recovery_factor`, `ulcer_performance_index` and `omega` are `f64::INFINITY` for a
/// curve that gained and never drew down (or lost): their `crate::metrics` functions' house
/// sentinel. JSON writes it `null`, so those four carry [`TradingResults::profit_factor`]'s
/// null-as-infinity pair, and a stored or wired document holding one decodes where a bare `f64`
/// refused the whole document. Every other field is finite or `0.0` by its function's own guards.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TradingResultsExtendedMetrics {
    // --- the trade ledger's own statistics ---
    pub net_profit: f64,
    pub gross_profit: f64,
    pub gross_loss: f64,
    pub total_fees: f64,
    pub average_win: f64,
    pub average_loss: f64,
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
    #[serde(
        serialize_with = "vike_trade_ledger::serialize_f64_null_when_nonfinite",
        deserialize_with = "vike_trade_ledger::de_f64_null_as_infinity"
    )]
    pub calmar: f64,
    pub cagr: f64,
    #[serde(
        serialize_with = "vike_trade_ledger::serialize_f64_null_when_nonfinite",
        deserialize_with = "vike_trade_ledger::de_f64_null_as_infinity"
    )]
    pub recovery_factor: f64,
    pub ulcer_index: f64,
    #[serde(
        serialize_with = "vike_trade_ledger::serialize_f64_null_when_nonfinite",
        deserialize_with = "vike_trade_ledger::de_f64_null_as_infinity"
    )]
    pub ulcer_performance_index: f64,
    pub k_ratio: f64,
    pub risk_return_ratio: f64,
    pub returns_volatility: f64,
    pub returns_skewness: f64,
    pub returns_kurtosis: f64,
    pub tail_ratio: f64,
    #[serde(
        serialize_with = "vike_trade_ledger::serialize_f64_null_when_nonfinite",
        deserialize_with = "vike_trade_ledger::de_f64_null_as_infinity"
    )]
    pub omega: f64,
    pub value_at_risk_95: f64,
    pub expected_shortfall_95: f64,
}

/// The threshold [`crate::metrics::omega`] is computed against: a return of zero, i.e. "gains over
/// losses". A non-zero threshold is a different question (excess over a target), and picking one
/// here would bake a target nobody chose into every stored report.
pub const OMEGA_THRESHOLD: f64 = 0.0;

/// The confidence the stored VaR and expected shortfall are computed at. 95% is the tearsheet
/// convention [`crate::TradingResultsTable`] already prints at, and the two must match or the live
/// and backtest doors answer the same question with different tails.
pub const TAIL_CONFIDENCE: f64 = 0.95;

impl TradingResultsExtendedMetrics {
    /// Compose the long-form catalog from a raw [`TradingRecord`]. `periods_per_year` is the same
    /// annualization factor [`TradingResults::from_record`] was given, so Sharpe and Sortino are on
    /// one scale — passing a different one here is the `sqrt(24)` class of bug
    /// [`periods_per_year_for_interval`]'s doc records.
    ///
    /// The curve's returns are built ONCE here, and sorted once, for the nine metrics that read
    /// them: each is the same `crate::metrics` function over the same values its own equity-curve
    /// door would build (`extended_on_a_long_curve_is_every_door_bit_for_bit` holds that).
    ///
    /// A curve kept by time (`TradingRecord::sampling`) is annualized at its own step and its Calmar and recovery
    /// factor divide by the whole path's drawdown, as [`TradingResults::from_record`]'s are.
    pub fn from_result(r: &TradingRecord, periods_per_year: f64) -> Self {
        let periods_per_year = r.periods_per_year(periods_per_year);
        let eq = &r.equity_curve;
        let tr = &r.trades;
        let mdd = r.max_drawdown();
        let rets = metrics::returns(eq);
        let sorted = metrics::ascending(rets.clone());
        TradingResultsExtendedMetrics {
            net_profit: metrics::net_profit(tr),
            gross_profit: metrics::gross_profit(tr),
            gross_loss: metrics::gross_loss(tr),
            total_fees: metrics::total_fees(tr),
            average_win: metrics::average_win(tr),
            average_loss: metrics::average_loss(tr),
            largest_win: metrics::largest_win(tr),
            largest_loss: metrics::largest_loss(tr),
            payoff_ratio: metrics::payoff_ratio(tr),
            expected_payoff: metrics::expected_payoff(tr),
            consecutive_wins: metrics::consecutive_wins(tr),
            consecutive_losses: metrics::consecutive_losses(tr),
            sqn: metrics::sqn(tr),
            long_ratio: metrics::long_ratio(tr),
            sortino: metrics::sortino_of_returns(&rets, periods_per_year),
            calmar: metrics::calmar_of_drawdown(eq, periods_per_year, mdd),
            cagr: metrics::cagr(eq, periods_per_year),
            recovery_factor: metrics::recovery_factor_of_drawdown(eq, mdd),
            ulcer_index: metrics::ulcer_index(eq),
            ulcer_performance_index: metrics::ulcer_performance_index(eq, periods_per_year),
            k_ratio: metrics::k_ratio(eq),
            risk_return_ratio: metrics::risk_return_ratio_of_returns(&rets),
            returns_volatility: metrics::volatility_of_returns(&rets, periods_per_year),
            returns_skewness: vike_math::statistics::sample_skewness(&rets),
            returns_kurtosis: vike_math::statistics::sample_excess_kurtosis(&rets),
            tail_ratio: metrics::tail_ratio_of_sorted(&sorted),
            omega: metrics::omega_of_returns(&rets, OMEGA_THRESHOLD),
            value_at_risk_95: metrics::value_at_risk_of_sorted(&sorted, TAIL_CONFIDENCE),
            expected_shortfall_95: metrics::expected_shortfall_of_sorted(&sorted, TAIL_CONFIDENCE),
        }
    }

    /// The `crate::overfit::sharpe_moments` of the curve this block was composed from, read off it instead of derived
    /// again: `n_obs` is the length of the curve's `metrics::returns`, and the per-period Sharpe, skewness and excess
    /// kurtosis are this block's, the same functions over the same returns, so the same bits
    /// (`extended_on_a_long_curve_is_every_door_bit_for_bit` holds it).
    pub fn sharpe_moments(&self, n_obs: usize) -> crate::overfit::OverfitSharpeMoments {
        crate::overfit::OverfitSharpeMoments::of(
            self.risk_return_ratio,
            n_obs,
            self.returns_skewness,
            self.returns_kurtosis,
        )
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
            "average_win" => self.average_win,
            "average_loss" => self.average_loss,
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
