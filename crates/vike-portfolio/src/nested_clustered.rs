//! Nested Clustered Optimisation (López de Prado, *Machine Learning for Asset Managers*, 2020, ch. 7,
//! snippet 7.6 `optPort_nco`): optimise inside each cluster of correlated assets, then across the
//! clusters, so the instability a cluster of near-duplicates causes in one large inversion stays
//! inside that cluster.
//!
//! 1. Each cluster's own portfolio on its block of the covariance (the book's `optPort`):
//!    `Σₖ⁻¹1 / 1ᵀΣₖ⁻¹1`, or with expected returns `Σₖ⁻¹μₖ / 1ᵀΣₖ⁻¹μₖ` scaled whatever the sign of
//!    its sum, so a cluster the returns argue against is carried into the next step rather than
//!    refused (`crate::mean_variance`'s `scaled_solution`).
//! 2. The clusters as assets: the reduced covariance `Σ'ₖₗ = wₖᵀ Σₖₗ wₗ` and reduced returns
//!    `μ'ₖ = wₖᵀ μₖ`, optimised by `crate::mean_variance`'s `minimum_variance` or `maximum_sharpe`.
//! 3. Each asset's weight: its weight in its cluster times its cluster's weight. On a
//!    block-diagonal covariance with the blocks as clusters this is exactly the unnested answer.
//!
//! The clusters are an argument: `crate::hierarchical::clusters_by_silhouette` proposes them, and any
//! partition will do (the book's code hands each cluster the whole return vector; here each gets its
//! own assets' returns).

use vike_math::linear_algebra::Matrix;

use crate::mean_variance::{maximum_sharpe, minimum_variance, scaled_solution};
use crate::{assets, submatrix};

/// The Nested Clustered Optimisation weights of `covariance` over `clusters`: minimum variance when
/// `expected_excess_returns` is `None`, maximum Sharpe when it is given. Fully invested.
///
/// `None` when `covariance` is not square or holds a non-finite entry, `expected_excess_returns` is
/// not one finite number per asset, `clusters` is not a partition of the assets (each index once, no
/// empty cluster), or an optimisation inside a cluster or across them has no answer (see
/// `crate::mean_variance`'s `minimum_variance` and `maximum_sharpe`).
pub fn nested_clustered_optimisation(
    covariance: &Matrix,
    expected_excess_returns: Option<&[f64]>,
    clusters: &[Vec<usize>],
) -> Option<Vec<f64>> {
    let n = assets(covariance)?;
    let mut seen = vec![false; n];
    for cluster in clusters {
        if cluster.is_empty() {
            return None;
        }
        for &asset in cluster {
            if seen.get(asset) != Some(&false) {
                return None;
            }
            seen[asset] = true;
        }
    }
    if seen.contains(&false) {
        return None;
    }
    if let Some(returns) = expected_excess_returns
        && (returns.len() != n || returns.iter().any(|r| !r.is_finite()))
    {
        return None;
    }
    let inside: Vec<Vec<f64>> = clusters
        .iter()
        .map(|cluster| {
            let target: Vec<f64> = match expected_excess_returns {
                None => vec![1.0; cluster.len()],
                Some(returns) => cluster.iter().map(|&i| returns[i]).collect(),
            };
            Some(scaled_solution(&submatrix(covariance, cluster)?, &target)?.0)
        })
        .collect::<Option<_>>()?;
    let k = clusters.len();
    let mut reduced = Vec::with_capacity(k * k);
    for (a, weights_a) in clusters.iter().zip(&inside) {
        for (b, weights_b) in clusters.iter().zip(&inside) {
            let mut total = 0.0;
            for (&i, weight_i) in a.iter().zip(weights_a) {
                for (&j, weight_j) in b.iter().zip(weights_b) {
                    total += weight_i * covariance.get(i, j) * weight_j;
                }
            }
            reduced.push(total);
        }
    }
    let reduced_returns = expected_excess_returns.map(|returns| {
        clusters
            .iter()
            .zip(&inside)
            .map(|(cluster, weights)| {
                cluster.iter().zip(weights).fold(0.0, |total, (&i, w)| total + w * returns[i])
            })
            .collect::<Vec<f64>>()
    });
    let reduced = Matrix::from_row_major(k, k, reduced)?;
    let across = match reduced_returns {
        None => minimum_variance(&reduced)?,
        Some(returns) => maximum_sharpe(&reduced, &returns)?,
    };
    let mut weights = vec![0.0; n];
    for ((cluster, within), share) in clusters.iter().zip(&inside).zip(&across) {
        for (&asset, weight) in cluster.iter().zip(within) {
            weights[asset] = weight * share;
        }
    }
    Some(weights)
}

#[cfg(test)]
#[path = "nested_clustered_tests.rs"]
mod tests;
