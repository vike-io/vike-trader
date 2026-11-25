//! Portfolio construction and bet sizing: the weights a set of assets is held in, and the size of
//! one bet. Pure maths, with no clock, file, randomness or environment read, so a strategy FILE and a
//! research study FILE both compile against it and compute the same numbers (both templates name it:
//! `docs/decisions/0089-the-user-code-fence-is-held-by-name-not-by-rank.md`'s addendum of 2026-11-16).
//!
//! The linear algebra is not here. `vike_math::linear_algebra` is the workspace's one home for it
//! (decision 0141): every covariance or correlation argument is its `Matrix` (assets × assets, built
//! from a returns matrix by its `covariance_matrix`), every solve is its Cholesky and every spectrum its
//! symmetric eigen-decomposition. A covariance must be symmetric; a function that factors it reads its
//! lower triangle.
//!
//! - [`mean_variance`] — minimum variance and maximum Sharpe (Markowitz), in closed form and long-only
//!   by accelerated projected gradient.
//! - [`risk_parity`] — inverse volatility, inverse variance and equal risk contributions.
//! - [`hierarchical`] — Hierarchical Risk Parity (López de Prado, *Advances in Financial Machine
//!   Learning*, ch. 16) and the correlation clustering it shares with [`nested_clustered`].
//! - [`kmeans`] — k-means on points or on assets' correlations, deterministic (farthest-first start), with
//!   `k` chosen by the silhouette quality [`hierarchical`] ranks its cuts by: the workspace's one
//!   k-means, beside its one single-linkage tree.
//! - [`denoise`] — the Marchenko–Pastur edge, constant-residual denoising and detoning of a
//!   correlation or covariance matrix (López de Prado, *Machine Learning for Asset Managers*, ch. 2).
//! - [`nested_clustered`] — Nested Clustered Optimisation (*Machine Learning for Asset Managers*,
//!   ch. 7).
//! - [`kelly`] — the Kelly fraction of a binary bet and the Kelly weights of normal returns, both
//!   scalable to a fractional Kelly.
//! - [`bet_size`] — the bet size of a predicted probability and its discretisation (*Advances*,
//!   ch. 10).
//! - [`principal_components`] — latent factors of a returns matrix: principal components (explained
//!   variance, loadings) and Avellaneda–Lee eigenportfolios.
//! - [`sizing`] — the `PositionSizer` registry (the eight sizers): ONE entry intent turned into a
//!   quantity from an account's equity, by `vike_model::ContractValue`'s one sizing law.
//! - [`alpha_pool`] — many strategies' forecasts combined (Isichenko, *Quantitative Portfolio
//!   Management*, ch. 3; book W21): their alpha returns, mean-variance and hierarchical combining
//!   weights on the modules above, the pool's forecast, and the decay of a forecast's information.
//!
//! **Conventions.** A weight vector is in the input's asset order. A portfolio "fully invested"
//! sums to 1 (up to rounding); a Kelly weight is leverage and need not. An answer that does not exist
//! is `None`, never a `NaN` weight: a shape that does not fit, a non-finite input, a covariance that is
//! not positive definite where a solve needs one, a zero variance where a weight divides by it.
//!
//! **Deterministic.** No transcendental but `sqrt` (correctly rounded by IEEE 754) and the bet size's
//! normal CDF (`vike_math::distributions::normal_cdf`, `libm`'s `erf`; decision 0032), and every fold in index
//! order (a sum-to-one total, a mean and a covariance compensated, `vike_math`'s; the rest naive left
//! folds): the same input gives the same bits on every platform.
//!
//! **`sizing` beside the weights.** The sizers turn ONE entry intent into a quantity from an
//! account's equity. The other modules answer the step before: what fraction (a weight, a Kelly
//! fraction, a bet size) each asset gets. A strategy multiplies that fraction by its equity itself;
//! none of the eight sizers computes a portfolio weight, so neither half repeats the other.

use vike_math::linear_algebra::Matrix;

pub mod alpha_pool;
pub mod bet_size;
pub mod denoise;
pub mod hierarchical;
pub mod kelly;
pub mod kmeans;
pub mod mean_variance;
pub mod nested_clustered;
pub mod principal_components;
pub mod risk_parity;
pub mod sizing;

/// The number of assets of a square, non-empty matrix whose every entry is finite; `None` otherwise.
pub(crate) fn assets(matrix: &Matrix) -> Option<usize> {
    let n = matrix.rows();
    (n > 0 && matrix.columns() == n && matrix.values().iter().all(|v| v.is_finite())).then_some(n)
}

/// `matrix · vector`, through `Matrix::multiply`; `None` when the sizes do not fit.
pub(crate) fn product(matrix: &Matrix, vector: &[f64]) -> Option<Vec<f64>> {
    let column = Matrix::from_row_major(vector.len(), 1, vector.to_vec())?;
    Some(matrix.multiply(&column)?.values().to_vec())
}

/// The rows and columns `indices` of `matrix`, in that order.
pub(crate) fn submatrix(matrix: &Matrix, indices: &[usize]) -> Option<Matrix> {
    let rows: Vec<Vec<f64>> =
        indices.iter().map(|&i| indices.iter().map(|&j| matrix.get(i, j)).collect()).collect();
    Matrix::from_row_major(indices.len(), indices.len(), rows.concat())
}
