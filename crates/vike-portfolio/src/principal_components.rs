//! Principal components and eigenportfolios of a returns matrix (the book's latent-factor theme):
//! the statistical factors a set of assets' returns share, read off the eigen-decomposition of their
//! covariance or correlation matrix.
//!
//! The linear algebra is NOT here: `vike_math::linear_algebra`, the workspace's one home for it,
//! builds the matrix and decomposes it, and this module names what the eigenpairs mean. Every
//! `returns` argument holds one row per observation (a bar, a day) and one column per asset. Pure; no
//! clock, no file, no randomness.

use vike_math::linear_algebra::{Matrix, correlation_matrix, covariance_matrix, symmetric_eigen};
use vike_math::statistics::normalised_to_unit_sum;

/// The matrix [`principal_components`] decomposes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrincipalComponentBasis {
    /// The sample covariance: an asset with a larger variance weighs more.
    Covariance,
    /// The correlation, i.e. the components of the standardised returns: every asset weighs alike.
    Correlation,
}

/// A principal component analysis, from [`principal_components`].
#[derive(Clone, Debug, PartialEq)]
pub struct PrincipalComponents {
    /// The variance each component explains, largest first: the eigenvalues of the basis matrix.
    /// On the correlation basis they sum to the number of assets.
    pub explained_variance: Vec<f64>,
    /// Each explained variance over their sum: the component's share of the total variance.
    pub explained_variance_ratio: Vec<f64>,
    /// Assets × components: column `k` is component `k`'s unit loading vector, the weight of each
    /// asset in it (scikit-learn's `components_`, transposed). Its largest-magnitude entry is
    /// positive. Scale column `k` by `sqrt(explained_variance[k])` for the asset–component
    /// covariances.
    pub loadings: Matrix,
}

/// The principal components of `returns` on the chosen basis.
///
/// `None` when the basis matrix does not exist (fewer than two observations, no asset, a non-finite
/// return, and on the correlation basis an asset whose return never moves), when the decomposition
/// does not finish, or when the total variance is not a positive finite number.
pub fn principal_components(
    returns: &Matrix,
    basis: PrincipalComponentBasis,
) -> Option<PrincipalComponents> {
    let matrix = match basis {
        PrincipalComponentBasis::Covariance => covariance_matrix(returns)?,
        PrincipalComponentBasis::Correlation => correlation_matrix(returns)?,
    };
    let eigen = symmetric_eigen(&matrix)?;
    let explained_variance_ratio = normalised_to_unit_sum(&eigen.values)?;
    Some(PrincipalComponents {
        explained_variance: eigen.values,
        explained_variance_ratio,
        loadings: eigen.vectors,
    })
}

/// One eigenportfolio, from [`eigenportfolios`].
#[derive(Clone, Debug, PartialEq)]
pub struct Eigenportfolio {
    /// The correlation matrix's eigenvalue, which is also the sample variance of [`Self::returns`].
    pub eigenvalue: f64,
    /// One weight per asset: the eigenvector's entry over that asset's return standard deviation,
    /// `v_i / σ_i` (the paper's `Q_i`), an amount held per unit of the factor.
    pub weights: Vec<f64>,
    /// The portfolio's return at each observation, `Σ_i weights_i · R_ti` (the paper's `F`).
    pub returns: Vec<f64>,
}

/// The first `count` eigenportfolios of `returns`, largest eigenvalue first: the eigenvectors of
/// the returns' CORRELATION matrix, each entry divided by its asset's standard deviation (Avellaneda
/// and Lee, "Statistical arbitrage in the U.S. equities market", Quantitative Finance 10(7), 2010,
/// section 2.1). Their returns are uncorrelated with each other in sample. When every pair of
/// assets correlates positively the first is the market portfolio, every weight positive.
///
/// `None` where [`principal_components`] on the correlation basis is, or when `count` exceeds the
/// number of assets.
pub fn eigenportfolios(returns: &Matrix, count: usize) -> Option<Vec<Eigenportfolio>> {
    let assets = returns.columns();
    if count > assets {
        return None;
    }
    let components = principal_components(returns, PrincipalComponentBasis::Correlation)?;
    let covariance = covariance_matrix(returns)?;
    let deviations: Vec<f64> = (0..assets).map(|i| covariance.get(i, i).sqrt()).collect();
    let portfolios = (0..count)
        .map(|j| {
            let weights: Vec<f64> =
                components.loadings.column(j).iter().zip(&deviations).map(|(v, d)| v / d).collect();
            let portfolio_returns = (0..returns.rows())
                .map(|t| {
                    let mut total = 0.0;
                    for (weight, value) in weights.iter().zip(returns.row(t)) {
                        total += weight * value;
                    }
                    total
                })
                .collect();
            Eigenportfolio {
                eigenvalue: components.explained_variance[j],
                weights,
                returns: portfolio_returns,
            }
        })
        .collect();
    Some(portfolios)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two assets driven by one common factor `[1, 1, -1, -1]` plus uncorrelated idiosyncratic
    /// moves `[1, -1, 1, -1]` and `[1, -1, -1, 1]`, each of variance `4/3`: the covariance is
    /// `(4/3) [[2, 1], [1, 2]]`, so the components explain `4` and `4/3` (75% and 25%) along
    /// `(1, 1) / √2` and `(1, -1) / √2`.
    fn two_assets() -> Matrix {
        Matrix::from_rows(&[vec![2.0, 2.0], vec![0.0, 0.0], vec![0.0, -2.0], vec![-2.0, 0.0]])
            .unwrap()
    }

    fn near(got: f64, want: f64) -> bool {
        (got - want).abs() <= 1e-14 * want.abs().max(1.0)
    }

    /// A fixed uniform sequence in `[-0.5, 0.5)` (a 64-bit LCG, so the test needs no `rand`).
    fn noise(seed: u64, n: usize) -> Vec<f64> {
        let mut state = seed;
        (0..n)
            .map(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                (state >> 11) as f64 / (1u64 << 53) as f64 - 0.5
            })
            .collect()
    }

    #[test]
    fn the_components_of_two_assets_by_hand() {
        let pca = principal_components(&two_assets(), PrincipalComponentBasis::Covariance).unwrap();
        assert!(
            near(pca.explained_variance[0], 4.0) && near(pca.explained_variance[1], 4.0 / 3.0),
            "{pca:?}"
        );
        assert!(
            near(pca.explained_variance_ratio[0], 0.75)
                && near(pca.explained_variance_ratio[1], 0.25)
        );
        let half = 0.5f64.sqrt();
        assert!(pca.loadings.column(0).iter().all(|v| near(*v, half)), "{pca:?}");
        let second = pca.loadings.column(1);
        assert!(near(second[0].abs(), half) && near(second[0], -second[1]), "{pca:?}");
        // The correlation basis: `[[1, 0.5], [0.5, 1]]`, so 1.5 and 0.5, the same shares.
        let pca =
            principal_components(&two_assets(), PrincipalComponentBasis::Correlation).unwrap();
        assert!(
            near(pca.explained_variance[0], 1.5) && near(pca.explained_variance[1], 0.5),
            "{pca:?}"
        );
        assert!(near(pca.explained_variance_ratio[0], 0.75));
    }

    /// Both standard deviations are `sqrt(8/3)`, so the market weights are `(1/√2) / sqrt(8/3) = √3/4`
    /// and the returns `√3/4 · (4, 0, -2, -2)`, whose sample variance is `4.5 / 3 = 1.5`, the
    /// eigenvalue.
    #[test]
    fn the_market_eigenportfolio_by_hand() {
        let portfolios = eigenportfolios(&two_assets(), 1).unwrap();
        let market = &portfolios[0];
        let weight = 3f64.sqrt() / 4.0;
        assert!(
            near(market.eigenvalue, 1.5) && market.weights.iter().all(|w| near(*w, weight)),
            "{market:?}"
        );
        let root = 3f64.sqrt();
        for (got, want) in market.returns.iter().zip([root, 0.0, -root / 2.0, -root / 2.0]) {
            assert!(near(*got, want), "{market:?}");
        }
    }

    /// Five assets on one common factor: the eigenportfolios' returns are uncorrelated in sample,
    /// each with the variance of its eigenvalue; the market one is long every asset; the shares sum
    /// to one and the correlation basis's variances to the number of assets.
    #[test]
    fn eigenportfolio_returns_are_uncorrelated_with_their_eigenvalue_as_variance() {
        let (rows, assets) = (200, 5);
        let common = noise(1, rows);
        let own = noise(2, rows * assets);
        let mut values = Vec::with_capacity(rows * assets);
        for t in 0..rows {
            for i in 0..assets {
                values.push(0.01 * (common[t] + 0.5 * own[t * assets + i]) * (1.0 + i as f64));
            }
        }
        let returns = Matrix::from_row_major(rows, assets, values).unwrap();
        let portfolios = eigenportfolios(&returns, assets).unwrap();
        assert!(portfolios[0].weights.iter().all(|w| *w > 0.0), "{:?}", portfolios[0]);
        let mut columns = Vec::with_capacity(rows * assets);
        for t in 0..rows {
            for portfolio in &portfolios {
                columns.push(portfolio.returns[t]);
            }
        }
        let covariance =
            covariance_matrix(&Matrix::from_row_major(rows, assets, columns).unwrap()).unwrap();
        for (j, portfolio) in portfolios.iter().enumerate() {
            assert!((covariance.get(j, j) - portfolio.eigenvalue).abs() < 1e-10, "{j}");
            for k in 0..j {
                assert!(covariance.get(j, k).abs() < 1e-10, "{j} {k}");
            }
        }
        let pca = principal_components(&returns, PrincipalComponentBasis::Correlation).unwrap();
        let (mut shares, mut variances) = (0.0, 0.0);
        for (share, variance) in pca.explained_variance_ratio.iter().zip(&pca.explained_variance) {
            shares += share;
            variances += variance;
        }
        assert!((shares - 1.0).abs() < 1e-14 && (variances - assets as f64).abs() < 1e-12);
    }

    #[test]
    fn no_components_without_an_answer() {
        let constant =
            Matrix::from_rows(&[vec![1.0, 0.5], vec![2.0, 0.5], vec![4.0, 0.5]]).unwrap();
        assert!(principal_components(&constant, PrincipalComponentBasis::Correlation).is_none());
        assert!(eigenportfolios(&constant, 1).is_none());
        // The covariance basis has an answer: one asset carries all the variance.
        let pca = principal_components(&constant, PrincipalComponentBasis::Covariance).unwrap();
        assert!(
            near(pca.explained_variance_ratio[0], 1.0) && pca.explained_variance[1] == 0.0,
            "{pca:?}"
        );
        let flat = Matrix::from_rows(&[vec![0.5, 0.5], vec![0.5, 0.5]]).unwrap();
        assert!(principal_components(&flat, PrincipalComponentBasis::Covariance).is_none());
        assert!(eigenportfolios(&two_assets(), 3).is_none());
        assert_eq!(eigenportfolios(&two_assets(), 0).unwrap(), vec![]);
    }
}
