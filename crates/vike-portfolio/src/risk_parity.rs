//! Risk-based weights, which need no expected return: inverse volatility, inverse variance and equal
//! risk contributions ("risk parity"; Maillard, Roncalli and Teïletche, "The Properties of Equally
//! Weighted Risk Contribution Portfolios", 2010).
//!
//! Asset `i`'s risk contribution to a portfolio `w` is `wᵢ (Σw)ᵢ` ([`risk_contributions`]); the
//! contributions sum to the portfolio variance `wᵀΣw`. [`risk_parity`] makes them all equal. With
//! uncorrelated assets, and for any two assets, that is the inverse-volatility portfolio.

use vike_math::linear_algebra::Matrix;
use vike_math::statistics::normalised_to_unit_sum;

use crate::{assets, product};

/// [`risk_parity`] stops when a sweep moves no weight by more than this share of the largest.
const RISK_PARITY_TOLERANCE: f64 = 1e-14;

/// The most coordinate sweeps [`risk_parity`] takes before it answers `None`.
const RISK_PARITY_MAXIMUM_SWEEPS: usize = 10_000;

/// `wᵢ ∝ 1 / σᵢ`, fully invested.
///
/// `None` when `covariance` is not square, holds a non-finite entry, or has a variance that is not
/// positive.
pub fn inverse_volatility(covariance: &Matrix) -> Option<Vec<f64>> {
    normalised_to_unit_sum(
        &variances(covariance)?.iter().map(|v| 1.0 / v.sqrt()).collect::<Vec<_>>(),
    )
}

/// `wᵢ ∝ 1 / σᵢ²`, fully invested: the minimum-variance portfolio when the assets are uncorrelated,
/// and the weights Hierarchical Risk Parity spreads inside a cluster (López de Prado's `getIVP`).
///
/// `None` where [`inverse_volatility`] is.
pub fn inverse_variance(covariance: &Matrix) -> Option<Vec<f64>> {
    normalised_to_unit_sum(&variances(covariance)?.iter().map(|v| 1.0 / v).collect::<Vec<_>>())
}

/// Each asset's risk contribution `wᵢ (Σw)ᵢ`; they sum to `wᵀΣw`.
///
/// `None` when `covariance` is not square or holds a non-finite entry, or `weights` is not one per
/// asset.
pub fn risk_contributions(covariance: &Matrix, weights: &[f64]) -> Option<Vec<f64>> {
    assets(covariance)?;
    let marginal = product(covariance, weights)?;
    Some(weights.iter().zip(&marginal).map(|(w, m)| w * m).collect())
}

/// The fully invested, long-only portfolio whose assets contribute equal risk.
///
/// Its `y` minimises the strictly convex `½ yᵀΣy − (1/n) Σ ln yᵢ` over `y > 0`, where `yᵢ (Σy)ᵢ = 1/n`
/// for every asset; `w = y / Σy`. Solved by cyclical coordinate descent from the inverse-volatility
/// point: each coordinate's minimum is the positive root of `σᵢ² yᵢ² + cᵢ yᵢ − 1/n = 0`, with
/// `cᵢ = Σⱼ≠ᵢ σᵢⱼ yⱼ` (Griveau-Billion, Richard and Roncalli, "A Fast Algorithm for Computing
/// High-dimensional Risk Parity Portfolios", 2013), written `2b / (c + √(c² + 4σ²b))` when `c > 0` so
/// the root does not cancel.
///
/// `None` where [`inverse_volatility`] is, and when `RISK_PARITY_MAXIMUM_SWEEPS` sweeps did not
/// settle (a covariance that is not positive semi-definite can do that).
pub fn risk_parity(covariance: &Matrix) -> Option<Vec<f64>> {
    let variances = variances(covariance)?;
    let n = variances.len();
    let budget = 1.0 / n as f64;
    let mut y: Vec<f64> = variances.iter().map(|v| 1.0 / v.sqrt()).collect();
    for _ in 0..RISK_PARITY_MAXIMUM_SWEEPS {
        let mut moved = 0.0_f64;
        for i in 0..n {
            let row = covariance.row(i);
            let mut others = 0.0;
            for (j, (entry, weight)) in row.iter().zip(&y).enumerate() {
                if j != i {
                    others += entry * weight;
                }
            }
            let root = (others * others + 4.0 * variances[i] * budget).sqrt();
            let next = if others > 0.0 {
                2.0 * budget / (others + root)
            } else {
                (root - others) / (2.0 * variances[i])
            };
            moved = moved.max((next - y[i]).abs());
            y[i] = next;
        }
        let largest = y.iter().fold(0.0, |most: f64, v| most.max(*v));
        if moved <= RISK_PARITY_TOLERANCE * largest {
            return normalised_to_unit_sum(&y);
        }
    }
    None
}

/// The diagonal of a square, finite covariance whose every variance is positive.
fn variances(covariance: &Matrix) -> Option<Vec<f64>> {
    let n = assets(covariance)?;
    let variances: Vec<f64> = (0..n).map(|i| covariance.get(i, i)).collect();
    variances.iter().all(|v| *v > 0.0).then_some(variances)
}

#[cfg(test)]
#[path = "risk_parity_tests.rs"]
mod tests;
