//! Pair discovery: which of many assets have a spread worth testing, found from the data without a list of
//! candidates written in advance.
//!
//! Testing every pair of `n` assets is `n (n − 1) / 2` cointegration tests (twice that, the test not being
//! symmetric), and at a 5% level one in twenty of them "finds" a pair by chance. Two ways narrow the search
//! to assets that already move together:
//!
//! - **Clustering the returns** ([`cluster_returns`]). The correlation of the assets' returns becomes a distance
//!   and `vike_portfolio`'s clusterings group them: `hierarchical::clusters_by_silhouette` (the single-linkage tree)
//!   or `kmeans::kmeans_clusters`, the number of clusters chosen by the silhouette. [`cointegrated_pairs`] then
//!   runs the Engle–Granger test only INSIDE each cluster. It takes the clusters as an ARGUMENT, so a caller with
//!   its own groups (sectors, a venue's families) hands those in instead.
//! - **PCA residuals** ([`factor_residuals`]; Avellaneda and Lee, "Statistical arbitrage in the U.S.
//!   equities market", Quantitative Finance 10(7), 2010): regress each asset's returns on the returns of the
//!   first `k` eigenportfolios (the market and the next statistical factors); what is left is the part of the
//!   asset that no factor explains, and its running sum is a spread of the asset against the factors, to read
//!   the way a pair's is ([`crate::spread::single_series_signals`]). The eigenportfolios are the caller's
//!   (`vike_portfolio::principal_components::eigenportfolios`): their RETURNS are an argument, so this module
//!   names no eigen-decomposition of its own.
//!
//! **A screen is not a test.** Fewer tests mean fewer chance finds, not none: a pair that passes here has
//! passed ONE test among many, on the data the screen saw. The honest check is the one a study makes: discover
//! on an earlier window and judge the pair on a later one the screen never saw. Nothing in this module looks at
//! a later bar than the matrix it is given.

use vike_math::linear_algebra::{Matrix, correlation_matrix};
use vike_math::statistics::{centred_sum_of_squares, mean};
use vike_portfolio::hierarchical::clusters_by_silhouette;
use vike_portfolio::kmeans::kmeans_clusters;

use crate::cointegration::critical_values::{CriticalValues, Significance};
use crate::cointegration::{engle_granger, fitted_residuals};

/// How [`cluster_returns`] groups assets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clustering {
    /// Cut the single-linkage tree (`vike_portfolio::hierarchical::clusters_by_silhouette`).
    SingleLinkage,
    /// k-means on each asset's row of the correlation distance (`vike_portfolio::kmeans::kmeans_clusters`).
    KMeans,
}

/// The clusters of assets whose returns move together: `returns` has one row per bar and one column per asset, and
/// the answer lists each cluster's asset (column) indices in order, the clusters in the order of their first asset.
/// At most `max_clusters` clusters; their number is the one with the best silhouette.
///
/// `None` when the correlation of the returns does not exist (fewer than two rows, a return that is not finite, an
/// asset whose return never moves) or no cluster count is admissible (a silhouette needs `2 ≤ k ≤ assets − 1`).
pub fn cluster_returns(
    returns: &Matrix,
    max_clusters: usize,
    method: Clustering,
) -> Option<Vec<Vec<usize>>> {
    let correlation = correlation_matrix(returns)?;
    match method {
        Clustering::SingleLinkage => clusters_by_silhouette(&correlation, max_clusters),
        Clustering::KMeans => kmeans_clusters(&correlation, max_clusters),
    }
}

/// One pair that passed the Engle–Granger screen: `first` is regressed on `second`, so the spread is
/// `first − intercept − hedge_ratio · second`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PairCandidate {
    /// The column of the dependent asset.
    pub first: usize,
    /// The column of the regressor.
    pub second: usize,
    pub intercept: f64,
    pub hedge_ratio: f64,
    /// The residual's Dickey–Fuller statistic; the more negative, the stronger the evidence.
    pub statistic: f64,
    pub critical_values: CriticalValues,
}

/// The pairs inside each of `clusters` that the Engle–Granger test finds cointegrated at `level`, the most
/// significant first. `log_prices` has one row per bar and one column per asset (the columns `clusters`
/// name); both orientations of each pair are tried (the test is not symmetric) and the one with the more
/// negative statistic is kept. A cluster of one asset has no pair; to test every pair, pass one cluster
/// holding every column.
///
/// A pair the test cannot run on (a series that never moves, two that repeat each other) is left
/// out, and so is a column no cluster covers.
pub fn cointegrated_pairs(
    log_prices: &Matrix,
    clusters: &[Vec<usize>],
    lags: usize,
    level: Significance,
) -> Vec<PairCandidate> {
    let assets = log_prices.columns();
    let mut found = Vec::new();
    for cluster in clusters {
        for (position, &a) in cluster.iter().enumerate() {
            for &b in &cluster[position + 1..] {
                if a >= assets || b >= assets {
                    continue;
                }
                let best = [(a, b), (b, a)]
                    .into_iter()
                    .filter_map(|(first, second)| {
                        let regressor = Matrix::from_row_major(
                            log_prices.rows(),
                            1,
                            log_prices.column(second),
                        )?;
                        let test = engle_granger(&log_prices.column(first), &regressor, lags)?;
                        Some((first, second, test))
                    })
                    .min_by(|x, y| x.2.statistic.total_cmp(&y.2.statistic));
                if let Some((first, second, test)) = best
                    && test.cointegrated(level)
                {
                    found.push(PairCandidate {
                        first,
                        second,
                        intercept: test.coefficients[0],
                        hedge_ratio: test.coefficients[1],
                        statistic: test.statistic,
                        critical_values: test.critical_values,
                    });
                }
            }
        }
    }
    found.sort_by(|x, y| {
        x.statistic
            .total_cmp(&y.statistic)
            .then(x.first.cmp(&y.first))
            .then(x.second.cmp(&y.second))
    });
    found
}

/// What is left of each asset's returns after the statistical factors, from [`factor_residuals`].
#[derive(Clone, Debug, PartialEq)]
pub struct FactorResiduals {
    /// One row per bar, one column per asset: the return less its fitted factor exposure and
    /// intercept. Each column sums to zero and is uncorrelated with every factor, in sample.
    pub residuals: Matrix,
    /// The running sum of each column of `residuals`: the asset's spread against the factors (Avellaneda and
    /// Lee's auxiliary process `X`). It ends at zero, the residuals summing to zero, so read it through a
    /// rolling window (the spread indicators' z-score and half-life, with a zero second leg).
    pub spreads: Matrix,
    /// The share of each asset's return variance the factors explain, `1 − RSS / TSS`.
    pub r_squared: Vec<f64>,
}

/// Regress each asset's returns (one row per bar, one column per asset) on `factor_returns` (one row per bar,
/// one column per factor: the returns of the first eigenportfolios of the same assets, Avellaneda and Lee
/// section 2.1, which the caller computes) and an intercept, by least squares.
///
/// `None` when there is no factor, a factor count that exceeds the number of assets, rows that differ
/// between the two matrices, not more rows than `factors + 1`, a non-finite value, or a regression with
/// no unique answer (a factor that never moves, two that repeat each other). The factors are whatever the
/// caller estimated: one fitted on the whole matrix handed in makes the residuals in-sample, and a caller judging
/// them out of sample fits the factors on an earlier window itself.
pub fn factor_residuals(returns: &Matrix, factor_returns: &Matrix) -> Option<FactorResiduals> {
    let (rows, assets, factors) = (returns.rows(), returns.columns(), factor_returns.columns());
    if factors == 0 || factors > assets || factor_returns.rows() != rows || rows <= factors + 1 {
        return None;
    }
    let mut design = Vec::with_capacity(rows * (factors + 1));
    for row in factor_returns.values().chunks_exact(factors) {
        design.push(1.0);
        design.extend_from_slice(row);
    }
    let design = Matrix::from_row_major(rows, factors + 1, design)?;
    let mut residual_values = vec![0.0; rows * assets];
    let mut spread_values = vec![0.0; rows * assets];
    let mut r_squared = Vec::with_capacity(assets);
    for j in 0..assets {
        let observed = returns.column(j);
        let (_, residuals) = fitted_residuals(&design, &observed)?;
        let total =
            centred_sum_of_squares(observed.iter().copied(), mean(observed.iter().copied()));
        let mut unexplained = 0.0;
        for residual in &residuals {
            unexplained += residual * residual;
        }
        r_squared.push(1.0 - unexplained / total);
        let mut running = 0.0;
        for (t, residual) in residuals.into_iter().enumerate() {
            running += residual;
            residual_values[t * assets + j] = residual;
            spread_values[t * assets + j] = running;
        }
    }
    Some(FactorResiduals {
        residuals: Matrix::from_row_major(rows, assets, residual_values)?,
        spreads: Matrix::from_row_major(rows, assets, spread_values)?,
        r_squared,
    })
}
