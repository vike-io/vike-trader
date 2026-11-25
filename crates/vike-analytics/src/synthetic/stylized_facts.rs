//! The STYLIZED FACTS of a bar path's returns (R. Cont, "Empirical properties of asset returns:
//! stylized facts and statistical issues", Quantitative Finance 1, 2001): the numbers that tell a
//! generator that only matches a market's volatility from one that matches the market. Three facts:
//! - the distribution's shape: its skewness, its heavy tails (excess kurtosis) and its tail quantiles;
//! - the near-absence of LINEAR autocorrelation of returns;
//! - VOLATILITY CLUSTERING: the positive, slowly decaying autocorrelation of absolute returns.
//!
//! One call per path, so a study lays the paths of several generators (the price models, the
//! bootstrap, a learned model) beside the source they imitate. Every number is one this crate already
//! computes, on the close-to-close SIMPLE returns (`crate::metrics::returns`): the mean and sample
//! deviation (`vike_math::statistics::mean_variance`), `crate::metrics::returns_skewness` and
//! `crate::metrics::returns_kurtosis`, `vike_math::statistics::percentile` and `vike_math::time_series::autocorrelation`.

use vike_marketdata::Bar;

use crate::metrics::{returns, returns_kurtosis, returns_skewness};
use vike_math::statistics::{mean_variance, percentile};
use vike_math::time_series::autocorrelation;

/// The probabilities of [`StylizedFacts::quantiles`], in order.
pub const STYLIZED_QUANTILES: [f64; 5] = [0.01, 0.05, 0.5, 0.95, 0.99];

/// A path's stylized facts (the module doc).
#[derive(Debug, Clone, PartialEq)]
pub struct StylizedFacts {
    /// How many returns the path has: one fewer than its bars.
    pub returns: usize,
    /// The mean return; `NaN` under two returns.
    pub mean: f64,
    /// The sample standard deviation of the returns; `NaN` under two returns.
    pub standard_deviation: f64,
    /// The bias-corrected skewness (`crate::metrics::returns_skewness`; 0 under three returns).
    pub skewness: f64,
    /// The bias-corrected excess kurtosis, 0 for a normal law (`crate::metrics::returns_kurtosis`; 0
    /// under four returns).
    pub excess_kurtosis: f64,
    /// The return at each of [`STYLIZED_QUANTILES`], interpolated (`vike_math::statistics::percentile`);
    /// `NaN` without a return.
    pub quantiles: [f64; 5],
    /// The autocorrelation of the returns at lags `1..=max_lag`.
    pub autocorrelation: Vec<f64>,
    /// The autocorrelation of the ABSOLUTE returns at lags `1..=max_lag`: volatility clustering.
    pub absolute_autocorrelation: Vec<f64>,
}

/// The stylized facts of `bars`' close-to-close returns, autocorrelations to `max_lag`.
pub fn stylized_facts(bars: &[Bar], max_lag: usize) -> StylizedFacts {
    let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
    let path_returns = returns(&closes);
    let (mean, variance) = if path_returns.len() < 2 {
        (f64::NAN, f64::NAN)
    } else {
        mean_variance(path_returns.iter().copied(), 1.0)
    };
    let mut sorted = path_returns.clone();
    sorted.sort_by(f64::total_cmp);
    let quantiles = STYLIZED_QUANTILES
        .map(|q| if sorted.is_empty() { f64::NAN } else { percentile(&sorted, q) });
    let absolute: Vec<f64> = path_returns.iter().map(|r| r.abs()).collect();
    StylizedFacts {
        returns: path_returns.len(),
        mean,
        standard_deviation: variance.sqrt(),
        skewness: returns_skewness(&closes),
        excess_kurtosis: returns_kurtosis(&closes),
        quantiles,
        autocorrelation: autocorrelation(&path_returns, max_lag),
        absolute_autocorrelation: autocorrelation(&absolute, max_lag),
    }
}

#[cfg(test)]
mod tests {
    use super::super::{SyntheticGrid, SyntheticModel, SyntheticRegime, model_bars};
    use super::*;

    fn grid(bars: usize) -> SyntheticGrid {
        SyntheticGrid { start_ms: 0, step_ms: 60_000, bars }
    }

    #[test]
    fn a_short_path_has_the_hand_computed_facts() {
        // Closes 100, 110, 99, 99: returns 0.1, -0.1, 0.
        let bars: Vec<Bar> = [100.0, 110.0, 99.0, 99.0]
            .into_iter()
            .map(|c| Bar::ohlcv(0, c, c, c, c, 1.0))
            .collect();
        let facts = stylized_facts(&bars, 2);
        assert_eq!(facts.returns, 3);
        assert!(facts.mean.abs() < 1e-15, "{facts:?}");
        assert!((facts.standard_deviation - 0.1).abs() < 1e-12, "{facts:?}");
        assert!((facts.quantiles[2] - 0.0).abs() < 1e-15, "{facts:?}");
        assert!((facts.quantiles[0] + 0.098).abs() < 1e-12, "{facts:?}");
        // Returns 0.1, -0.1, 0 about mean 0: gamma_1 = -0.01 / 3 over gamma_0 = 0.02 / 3.
        assert!((facts.autocorrelation[0] + 0.5).abs() < 1e-12, "{facts:?}");
        assert_eq!(facts.autocorrelation.len(), 2);
        assert!(stylized_facts(&bars[..1], 1).mean.is_nan());
    }

    /// The two facts a formula gets wrong, on the two models that do and do not have them: a
    /// geometric Brownian path has neither heavy tails nor volatility clustering, while a regime
    /// switch between a calm and a wild volatility has both.
    #[test]
    fn regimes_cluster_their_volatility_and_a_brownian_path_does_not() {
        let gbm = SyntheticModel::GeometricBrownianMotion { drift: 0.0, volatility: 0.002 };
        let regimes = SyntheticModel::RegimeSwitching {
            regimes: [
                SyntheticRegime { drift: 0.0, volatility: 0.001 },
                SyntheticRegime { drift: 0.0, volatility: 0.005 },
            ],
            switch_probability: [0.01, 0.03],
        };
        let brownian =
            stylized_facts(&model_bars(&gbm, &grid(20_000), 100.0, 1.0, 1, 4).unwrap(), 5);
        let switching =
            stylized_facts(&model_bars(&regimes, &grid(20_000), 100.0, 1.0, 1, 4).unwrap(), 5);
        // One standard error of an autocorrelation over 20,000 returns is about 0.007.
        assert!(brownian.absolute_autocorrelation.iter().all(|r| r.abs() < 0.03), "{brownian:?}");
        assert!(brownian.autocorrelation.iter().all(|r| r.abs() < 0.03), "{brownian:?}");
        assert!(brownian.excess_kurtosis.abs() < 0.2, "{brownian:?}");
        assert!(switching.absolute_autocorrelation.iter().all(|&r| r > 0.15), "{switching:?}");
        assert!(switching.autocorrelation.iter().all(|r| r.abs() < 0.03), "{switching:?}");
        assert!(switching.excess_kurtosis > 2.0, "{switching:?}");
    }
}
