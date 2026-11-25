//! Mean-variance portfolios (Markowitz, "Portfolio Selection", 1952): the minimum-variance and the
//! maximum-Sharpe portfolio, fully invested (the weights sum to 1).
//!
//! - **Unconstrained, in closed form**: [`minimum_variance`] is `Σ⁻¹1 / 1ᵀΣ⁻¹1` and [`maximum_sharpe`]
//!   (the tangency portfolio) `Σ⁻¹μ / 1ᵀΣ⁻¹μ`, each one Cholesky solve (López de Prado's `optPort`,
//!   *Machine Learning for Asset Managers*, snippet 7.4). A weight may be negative: a short.
//! - **Long-only, by projection**: [`long_only_minimum_variance`] minimises `wᵀΣw` over the simplex
//!   `{w ≥ 0, Σw = 1}`; [`long_only_maximum_sharpe`] minimises `yᵀΣy` over `{y ≥ 0, μᵀy = 1}` and
//!   rescales `w = y / Σy` (the standard change of variables that turns the Sharpe ratio, a ratio, into
//!   a quadratic programme: Cornuéjols and Tütüncü, *Optimization Methods in Finance*, 2007, section
//!   8.2). Both run accelerated projected gradient (FISTA, Beck and Teboulle 2009, with the gradient
//!   restart of O'Donoghue and Candès 2015) at step `1 / λ_max(Σ)`, each step projected exactly onto the
//!   feasible set, until a plain projected-gradient step moves no weight by more than `10⁻¹²` of the
//!   largest. The answer is the true constrained optimum, not the unconstrained one clipped.
//!
//! `expected_excess_returns` is each asset's expected return over the risk-free rate, in the
//! covariance's units and period.

use vike_math::linear_algebra::{Matrix, cholesky, dot, symmetric_eigen};
use vike_math::statistics::normalised_to_unit_sum;
use vike_math::summation::compensated_sum;

use crate::{assets, product};

/// A long-only solve stops when a projected-gradient step moves no weight by more than this share of
/// the largest weight.
const LONG_ONLY_TOLERANCE: f64 = 1e-12;

/// The most accelerated projected-gradient steps a long-only solve takes; reaching it means the
/// covariance is too ill-conditioned for the method, and the answer is `None`.
const LONG_ONLY_MAXIMUM_ITERATIONS: usize = 100_000;

/// The fully invested portfolio of least variance, `Σ⁻¹1 / 1ᵀΣ⁻¹1`.
///
/// `None` when `covariance` is not square, holds a non-finite entry or is not positive definite.
pub fn minimum_variance(covariance: &Matrix) -> Option<Vec<f64>> {
    let n = assets(covariance)?;
    // `1ᵀΣ⁻¹1 > 0` for any positive-definite `Σ`.
    Some(scaled_solution(covariance, &vec![1.0; n])?.0)
}

/// The fully invested portfolio of greatest Sharpe ratio, `Σ⁻¹μ / 1ᵀΣ⁻¹μ` (the tangency portfolio).
///
/// `None` where [`minimum_variance`] is, when `expected_excess_returns` is not one finite number per
/// asset, and when `1ᵀΣ⁻¹μ ≤ 0`: then the normalised `Σ⁻¹μ` is the portfolio of LEAST Sharpe ratio
/// (its Sharpe is `sign(1ᵀΣ⁻¹μ) · sqrt(μᵀΣ⁻¹μ)`), and no fully invested maximum exists.
pub fn maximum_sharpe(covariance: &Matrix, expected_excess_returns: &[f64]) -> Option<Vec<f64>> {
    let n = assets(covariance)?;
    if !one_finite_per_asset(expected_excess_returns, n) {
        return None;
    }
    let (weights, total) = scaled_solution(covariance, expected_excess_returns)?;
    (total > 0.0).then_some(weights)
}

/// `Σ⁻¹b / 1ᵀΣ⁻¹b` and `1ᵀΣ⁻¹b`, WHATEVER the sign of that sum (López de Prado's `optPort`: `b = 1`
/// for minimum variance, `b = μ` for maximum Sharpe). Nested Clustered Optimisation needs the sign
/// kept: a cluster whose `1ᵀΣ⁻¹μ` is negative is held short by the step across clusters, which is
/// what makes the nested answer the tangency portfolio again on a block-diagonal covariance. The sum
/// is compensated, as `vike_math::statistics::normalised_to_unit_sum`'s total is; that function is not
/// called because it refuses a negative total, which this one keeps.
///
/// `None` when `covariance` is not positive definite or the sum is zero or not finite.
pub(crate) fn scaled_solution(covariance: &Matrix, b: &[f64]) -> Option<(Vec<f64>, f64)> {
    let solution = cholesky(covariance)?.solve(b)?;
    let total = compensated_sum(solution.iter().copied());
    if total == 0.0 || !total.is_finite() {
        return None;
    }
    Some((solution.iter().map(|v| v / total).collect(), total))
}

/// The fully invested, long-only portfolio of least variance: `min wᵀΣw` over `w ≥ 0`, `Σw = 1`.
///
/// `None` when `covariance` is not square, holds a non-finite entry, has no positive eigenvalue, or
/// the solve did not finish in `LONG_ONLY_MAXIMUM_ITERATIONS` steps.
pub fn long_only_minimum_variance(covariance: &Matrix) -> Option<Vec<f64>> {
    let n = assets(covariance)?;
    projected_minimum(covariance, &vec![1.0; n])
}

/// The fully invested, long-only portfolio of greatest Sharpe ratio: `y` minimising `yᵀΣy` over
/// `y ≥ 0`, `μᵀy = 1`, rescaled to `w = y / Σy`.
///
/// `None` where [`long_only_minimum_variance`] is, when `expected_excess_returns` is not one finite
/// number per asset, and when no asset's is positive (no long-only portfolio has a positive excess
/// return).
pub fn long_only_maximum_sharpe(
    covariance: &Matrix,
    expected_excess_returns: &[f64],
) -> Option<Vec<f64>> {
    let n = assets(covariance)?;
    if !one_finite_per_asset(expected_excess_returns, n) {
        return None;
    }
    normalised_to_unit_sum(&projected_minimum(covariance, expected_excess_returns)?)
}

fn one_finite_per_asset(values: &[f64], n: usize) -> bool {
    values.len() == n && values.iter().all(|v| v.is_finite())
}

/// The `y` minimising `½ yᵀΣy` over `{y ≥ 0, aᵀy = 1}` (`a` = `budget`): FISTA from the projection of
/// the origin, restarted whenever its momentum points uphill, and stopped once a plain
/// projected-gradient step from the current point moves no coordinate by more than
/// [`LONG_ONLY_TOLERANCE`] of the largest (that step is the optimality residual: it is zero exactly at
/// the minimum).
fn projected_minimum(covariance: &Matrix, budget: &[f64]) -> Option<Vec<f64>> {
    let n = budget.len();
    let largest = *symmetric_eigen(covariance)?.values.first()?;
    if largest.is_nan() || largest <= 0.0 {
        return None;
    }
    let step = 1.0 / largest;
    let gradient_step = |point: &[f64]| -> Option<Vec<f64>> {
        let gradient = product(covariance, point)?;
        let moved: Vec<f64> = point.iter().zip(&gradient).map(|(p, g)| p - step * g).collect();
        project_onto_budget(&moved, budget)
    };
    let mut current = project_onto_budget(&vec![0.0; n], budget)?;
    let mut previous = current.clone();
    let mut momentum = 1.0_f64;
    for _ in 0..LONG_ONLY_MAXIMUM_ITERATIONS {
        let next_momentum = (1.0 + (1.0 + 4.0 * momentum * momentum).sqrt()) / 2.0;
        let carry = (momentum - 1.0) / next_momentum;
        let extrapolated: Vec<f64> =
            current.iter().zip(&previous).map(|(c, p)| c + carry * (c - p)).collect();
        let next = gradient_step(&extrapolated)?;
        let uphill = dot(
            &extrapolated.iter().zip(&next).map(|(e, x)| e - x).collect::<Vec<_>>(),
            &next.iter().zip(&current).map(|(x, c)| x - c).collect::<Vec<_>>(),
        );
        momentum = if uphill > 0.0 { 1.0 } else { next_momentum };
        previous = std::mem::replace(&mut current, next);
        let plain = gradient_step(&current)?;
        if largest_move(&plain, &current) <= LONG_ONLY_TOLERANCE * largest_magnitude(&current) {
            return Some(plain);
        }
    }
    None
}

fn largest_move(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).fold(0.0, |most, (x, y)| f64::max(most, (x - y).abs()))
}

fn largest_magnitude(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |most, v| f64::max(most, v.abs()))
}

/// The Euclidean projection of `point` onto `{y ≥ 0, aᵀy = 1}` (`a` = `budget`): by its optimality
/// conditions `yᵢ = max(0, xᵢ − τ aᵢ)`, where `τ` is the root of
/// `g(τ) = Σ aᵢ max(0, xᵢ − τ aᵢ) = 1`. `g` is continuous, piecewise linear with its breakpoints at
/// `xᵢ / aᵢ`, and never increasing (its slope is `−Σ aᵢ²` over the active terms), so a binary search
/// over the sorted breakpoints finds the piece holding the root and the root is solved on it exactly.
/// With `a = 1` it is the projection onto the simplex (Held, Wolfe and Crowder 1974; Duchi et al. 2008).
///
/// `None` when no `aᵢ` is positive: the set is empty.
fn project_onto_budget(point: &[f64], budget: &[f64]) -> Option<Vec<f64>> {
    if !budget.iter().any(|a| *a > 0.0) {
        return None;
    }
    // The active terms' `Σ aᵢxᵢ` and `Σ aᵢ²` at `tau`, so `g(tau) = first − tau · second`.
    let active = |tau: f64| {
        point
            .iter()
            .zip(budget)
            .filter(|(x, a)| **a != 0.0 && *x - tau * *a > 0.0)
            .fold((0.0, 0.0), |(first, second), (x, a)| (first + a * x, second + a * a))
    };
    let level = |tau: f64| {
        let (first, second) = active(tau);
        first - tau * second
    };
    let mut breakpoints: Vec<f64> =
        point.iter().zip(budget).filter(|(_, a)| **a != 0.0).map(|(x, a)| x / a).collect();
    breakpoints.sort_by(f64::total_cmp);
    breakpoints.dedup();
    // Breakpoints at or left of the root; at least one exists, since some `aᵢ` is positive.
    let left = breakpoints.partition_point(|b| level(*b) >= 1.0);
    let probe = match left {
        0 => breakpoints[0] - (1.0 + breakpoints[0].abs()),
        k if k == breakpoints.len() => breakpoints[k - 1] + (1.0 + breakpoints[k - 1].abs()),
        k => 0.5 * (breakpoints[k - 1] + breakpoints[k]),
    };
    let (first, second) = active(probe);
    if second <= 0.0 {
        return None;
    }
    let tau = (first - 1.0) / second;
    Some(point.iter().zip(budget).map(|(x, a)| f64::max(0.0, x - tau * a)).collect())
}

#[cfg(test)]
#[path = "mean_variance_tests.rs"]
mod tests;
