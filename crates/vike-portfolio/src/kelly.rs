//! The Kelly criterion: the stake that maximises the expected logarithm of wealth (Kelly, "A New
//! Interpretation of Information Rate", 1956), and its fractional form, the full stake scaled by
//! `fraction` (½ is "half Kelly": three quarters of the growth for half the volatility; Thorp, "The
//! Kelly Criterion in Blackjack, Sports Betting and the Stock Market", 2006).
//!
//! - [`kelly_binary`] — one bet with two outcomes: `f* = p / loss − (1 − p) / win`.
//! - [`kelly_weights`] — continuous-time, normally distributed returns: `f* = Σ⁻¹(μ − r)` (Merton
//!   1969; Thorp 2006, section 7). For one asset that is `(μ − r) / σ²`, a 1 × 1 covariance.
//!
//! A Kelly stake is LEVERAGE, not a budget: the weights need not sum to 1, one above 1 borrows and a
//! negative one is a short (or, for a bet, the other side).

use vike_math::linear_algebra::{Matrix, cholesky};

use crate::assets;

/// The Kelly fraction of wealth to stake on a bet that, per unit staked, gains `win` with probability
/// `probability` and loses `loss` otherwise, scaled by `fraction`: `fraction · (p / loss − (1 − p) /
/// win)`. With `loss = 1` it is the classic `p − q / b`. Negative when the bet has negative edge.
///
/// `None` when `probability` is outside `[0, 1]`, `win` or `loss` is not a positive finite number, or
/// `fraction` is not finite.
pub fn kelly_binary(probability: f64, win: f64, loss: f64, fraction: f64) -> Option<f64> {
    let positive = |v: f64| v > 0.0 && v.is_finite();
    if !(0.0..=1.0).contains(&probability) || !positive(win) || !positive(loss) {
        return None;
    }
    if !fraction.is_finite() {
        return None;
    }
    Some(fraction * (probability / loss - (1.0 - probability) / win))
}

/// The (fractional) Kelly weights `fraction · Σ⁻¹(μ − r)` of assets whose returns over the period are
/// jointly normal with covariance `covariance` and expected excess returns `expected_excess_returns`.
///
/// `None` when `covariance` is not square, holds a non-finite entry or is not positive definite,
/// `expected_excess_returns` is not one finite number per asset, or `fraction` is not finite.
pub fn kelly_weights(
    covariance: &Matrix,
    expected_excess_returns: &[f64],
    fraction: f64,
) -> Option<Vec<f64>> {
    let n = assets(covariance)?;
    if expected_excess_returns.len() != n || expected_excess_returns.iter().any(|r| !r.is_finite())
    {
        return None;
    }
    if !fraction.is_finite() {
        return None;
    }
    let full = cholesky(covariance)?.solve(expected_excess_returns)?;
    Some(full.iter().map(|f| fraction * f).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A coin at `p = 0.6` paying even money: `0.6 − 0.4 = 0.2`; half Kelly `0.1`. At 2 : 1 and
    /// `p = 0.4`: `0.4 − 0.6 / 2 = 0.1`. Losing half the stake instead of all of it, at even odds and
    /// `p = 0.5`: `0.5 / 0.5 − 0.5 / 1 = 0.5`.
    #[test]
    fn the_binary_kelly_fraction_by_hand() {
        assert!((kelly_binary(0.6, 1.0, 1.0, 1.0).unwrap() - 0.2).abs() < 1e-15);
        assert!((kelly_binary(0.6, 1.0, 1.0, 0.5).unwrap() - 0.1).abs() < 1e-15);
        assert!((kelly_binary(0.4, 2.0, 1.0, 1.0).unwrap() - 0.1).abs() < 1e-15);
        assert_eq!(kelly_binary(0.5, 1.0, 0.5, 1.0), Some(0.5));
        assert!(kelly_binary(0.4, 1.0, 1.0, 1.0).unwrap() < 0.0, "negative edge");
        assert!(kelly_binary(1.1, 1.0, 1.0, 1.0).is_none());
        assert!(kelly_binary(0.5, 0.0, 1.0, 1.0).is_none());
        assert!(kelly_binary(0.5, 1.0, f64::INFINITY, 1.0).is_none());
        assert!(kelly_binary(0.5, 1.0, 1.0, f64::NAN).is_none());
    }

    /// One asset, `μ − r = 0.1`, `σ² = 0.04`: `2.5`, and half Kelly `1.25`. Two uncorrelated assets
    /// are each sized alone; a correlated pair is not.
    #[test]
    fn the_continuous_kelly_weights_by_hand() {
        let one = Matrix::from_row_major(1, 1, vec![0.04]).unwrap();
        assert_eq!(kelly_weights(&one, &[0.1], 1.0), Some(vec![2.5]));
        assert_eq!(kelly_weights(&one, &[0.1], 0.5), Some(vec![1.25]));
        let pair = Matrix::from_row_major(2, 2, vec![0.04, 0.0, 0.0, 0.25]).unwrap();
        assert_eq!(kelly_weights(&pair, &[0.1, -0.05], 1.0), Some(vec![2.5, -0.2]));
        // `[[1, 0.5], [0.5, 1]]⁻¹ = [[4, −2], [−2, 4]] / 3`, so `(0.1, 0.1)` gives `(0.2, 0.2) / 3`.
        let correlated = Matrix::from_row_major(2, 2, vec![1.0, 0.5, 0.5, 1.0]).unwrap();
        let weights = kelly_weights(&correlated, &[0.1, 0.1], 1.0).unwrap();
        for w in &weights {
            assert!((w - 0.2 / 3.0).abs() < 1e-15, "{weights:?}");
        }
        assert!(kelly_weights(&pair, &[0.1], 1.0).is_none());
        assert!(kelly_weights(&pair, &[0.1, 0.1], f64::INFINITY).is_none());
        let singular = Matrix::from_row_major(2, 2, vec![1.0, 1.0, 1.0, 1.0]).unwrap();
        assert!(kelly_weights(&singular, &[0.1, 0.1], 1.0).is_none());
    }
}
