//! Denoising and detoning a correlation (or covariance) matrix (López de Prado, *Machine Learning for
//! Asset Managers*, 2020, ch. 2).
//!
//! A sample correlation matrix of `N` assets over `T` observations carries noise: the eigenvalues of a
//! pure-noise one spread over the Marchenko–Pastur law, up to the edge
//! `λ₊ = σ²(1 + √(N/T))²` ([`marchenko_pastur_upper_edge`]; `σ² = 1` for a correlation matrix of
//! pure noise). Eigenvalues above the edge are signal. Count them with the one eigen-decomposition,
//! `vike_math::linear_algebra::symmetric_eigen(&correlation)`'s values `> λ₊`, and hand the count to:
//!
//! - [`denoise_correlation`] — the constant-residual method (snippet 2.4, `denoisedCorr`): keep the
//!   signal eigenvalues, replace every noise eigenvalue by their average (so the trace is kept),
//!   rebuild `V Λ Vᵀ` and rescale it to a unit diagonal; [`denoise_covariance`] does the same to a
//!   covariance's correlation and scales it back by the standard deviations (`deNoiseCov`).
//! - [`detone`] — removes the market component(s), the largest eigenvalues (section 2.6): rebuild from
//!   the other components and rescale to a unit diagonal. The result is singular, meant for
//!   clustering, never for an optimiser that inverts it.
//!
//! The book fits `σ²` to the eigenvalues by a kernel density and a numerical minimiser
//! (`findMaxEval`); that fit is not built here: the caller chooses `σ²`.

use vike_math::linear_algebra::{Matrix, correlation_from_covariance, symmetric_eigen};

use crate::assets;

/// The upper edge of the Marchenko–Pastur law of the eigenvalues of a `variables`-asset noise
/// correlation matrix sampled over `observations`: `σ²(1 + √(variables / observations))²`.
///
/// `None` when `noise_variance` is not a positive finite number or either count is zero.
pub fn marchenko_pastur_upper_edge(
    noise_variance: f64,
    observations: usize,
    variables: usize,
) -> Option<f64> {
    if noise_variance.is_nan() || noise_variance <= 0.0 || noise_variance.is_infinite() {
        return None;
    }
    if observations == 0 || variables == 0 {
        return None;
    }
    let root = 1.0 + (variables as f64 / observations as f64).sqrt();
    Some(noise_variance * root * root)
}

/// The constant-residual denoised `correlation`, keeping its `signal_count` largest eigenvalues.
///
/// `None` when `correlation` is not square or holds a non-finite entry, `signal_count` exceeds the
/// number of assets, the decomposition does not finish, or the rebuilt matrix has a diagonal entry
/// that is not positive.
pub fn denoise_correlation(correlation: &Matrix, signal_count: usize) -> Option<Matrix> {
    let n = assets(correlation)?;
    if signal_count > n {
        return None;
    }
    let mut eigen = symmetric_eigen(correlation)?;
    if signal_count < n {
        let noise = &mut eigen.values[signal_count..];
        let average = noise.iter().fold(0.0, |total, v| total + v) / noise.len() as f64;
        noise.fill(average);
    }
    correlation_from_covariance(&eigen.rebuild_with(|value| value))
}

/// [`denoise_correlation`] applied to `covariance`'s correlation, scaled back by its standard
/// deviations: `σᵢ σⱼ ρ̃ᵢⱼ`.
///
/// `None` where [`denoise_correlation`] is, and when a variance is not positive.
pub fn denoise_covariance(covariance: &Matrix, signal_count: usize) -> Option<Matrix> {
    let denoised = denoise_correlation(&correlation_from_covariance(covariance)?, signal_count)?;
    let n = denoised.rows();
    let deviations: Vec<f64> = (0..n).map(|i| covariance.get(i, i).sqrt()).collect();
    let mut values = Vec::with_capacity(n * n);
    for (i, row_deviation) in deviations.iter().enumerate() {
        for (j, column_deviation) in deviations.iter().enumerate() {
            values.push(denoised.get(i, j) * (row_deviation * column_deviation));
        }
    }
    Matrix::from_row_major(n, n, values)
}

/// `correlation` without its `market_count` largest components, rescaled to a unit diagonal.
///
/// `None` when `correlation` is not square or holds a non-finite entry, `market_count` is not below
/// the number of assets, the decomposition does not finish, or an asset is all market (its rebuilt
/// variance is not positive).
pub fn detone(correlation: &Matrix, market_count: usize) -> Option<Matrix> {
    let n = assets(correlation)?;
    if market_count >= n {
        return None;
    }
    let mut eigen = symmetric_eigen(correlation)?;
    eigen.values[..market_count].fill(0.0);
    correlation_from_covariance(&eigen.rebuild_with(|value| value))
}

#[cfg(test)]
#[path = "denoise_tests.rs"]
mod tests;
