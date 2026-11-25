use vike_arbitrage::cointegration::critical_values::Significance;
use vike_arbitrage::pair_discovery::{
    Clustering, cluster_returns, cointegrated_pairs, factor_residuals,
};
use vike_math::linear_algebra::Matrix;

use crate::common::{noise, walk};

fn columns(series: &[Vec<f64>]) -> Matrix {
    let (n, k) = (series[0].len(), series.len());
    let values: Vec<f64> = (0..n).flat_map(|t| series.iter().map(move |c| c[t])).collect();
    Matrix::from_row_major(n, k, values).unwrap()
}

/// Asset 1 is twice asset 0 plus a stationary remainder; assets 2 and 3 are independent walks. Inside the clusters
/// `[0, 1]` and `[2, 3]` the screen finds the first pair, with its hedge ratio (2 when asset 1 is the dependent
/// one, one half the other way round), and never a pair across the clusters.
#[test]
fn the_screen_tests_inside_clusters_only() {
    let n = 400;
    let x = walk(1, n);
    let mut e = 0.0;
    let y: Vec<f64> = noise(2, n)
        .into_iter()
        .zip(&x)
        .map(|(u, x)| {
            e = 0.5 * e + 0.3 * u;
            2.0 * x + e
        })
        .collect();
    let prices = columns(&[x, y, walk(3, n), walk(4, n)]);

    let pairs = cointegrated_pairs(&prices, &[vec![0, 1], vec![2, 3]], 1, Significance::OnePercent);
    let first = pairs.iter().find(|p| p.first.min(p.second) == 0 && p.first.max(p.second) == 1);
    let first = first.expect("the cointegrated pair is found");
    let expected = if first.first == 1 { 2.0 } else { 0.5 };
    assert!((first.hedge_ratio - expected).abs() < 0.02, "{first:?}");
    assert!(first.statistic < first.critical_values.one_percent);
    for pair in &pairs {
        let same = |a: usize, b: usize| (a < 2) == (b < 2);
        assert!(same(pair.first, pair.second), "a pair across clusters: {pair:?}");
    }

    // One cluster of everything: the pair is the most significant, and the list is sorted by statistic.
    let all = cointegrated_pairs(&prices, &[vec![0, 1, 2, 3]], 1, Significance::OnePercent);
    assert_eq!((all[0].first.min(all[0].second), all[0].first.max(all[0].second)), (0, 1));
    for pair in all.windows(2) {
        assert!(pair[0].statistic <= pair[1].statistic);
    }
    // A cluster of one asset has no pair, and an out-of-range index is skipped, not a panic.
    assert!(
        cointegrated_pairs(&prices, &[vec![0], vec![7, 8]], 1, Significance::TenPercent).is_empty()
    );
}

/// Four assets that load on one common factor with their own noise: after the first eigenportfolio is taken
/// out, each residual column sums to zero, is uncorrelated with that factor, the factor explains most of
/// each asset, and each cumulative spread ends where it started, at zero.
#[test]
fn factor_residuals_are_what_the_factors_leave() {
    let factor = noise(7, 300);
    let loadings = [1.0, 0.8, 1.2, 0.6];
    let series: Vec<Vec<f64>> = loadings
        .iter()
        .enumerate()
        .map(|(i, loading)| {
            let own = noise(20 + i as u64, 300);
            (0..300).map(|t| loading * factor[t] + 0.15 * own[t]).collect()
        })
        .collect();
    let returns = columns(&series);
    // The factor's returns are the caller's (an eigenportfolio's, in a study); here it is the common series itself.
    let result = factor_residuals(&returns, &columns(std::slice::from_ref(&factor))).unwrap();
    assert_eq!((result.residuals.rows(), result.residuals.columns()), (300, 4));
    for j in 0..4 {
        let column = result.residuals.column(j);
        let (mut sum, mut cross) = (0.0, 0.0);
        for (r, f) in column.iter().zip(&factor) {
            sum += r;
            cross += r * f;
        }
        assert!(sum.abs() < 1e-9, "asset {j}: {sum}");
        assert!(cross.abs() < 1e-8, "asset {j}: {cross}");
        assert!(result.r_squared[j] > 0.8 && result.r_squared[j] < 1.0, "{:?}", result.r_squared);
        assert!(result.spreads.get(299, j).abs() < 1e-9);
    }
    // The spread is the running sum of the residual.
    let (residual, spread) = (result.residuals.column(0), result.spreads.column(0));
    assert_eq!(spread[0], residual[0]);
    assert_eq!(spread[1], residual[0] + residual[1]);
}

#[test]
fn factor_residuals_have_no_answer_for_an_impossible_request() {
    let returns = columns(&[noise(1, 40), noise(2, 40), noise(3, 40)]);
    let factors =
        |k: u64, rows: usize| columns(&(0..k).map(|i| noise(50 + i, rows)).collect::<Vec<_>>());
    assert!(
        factor_residuals(&returns, &Matrix::from_row_major(40, 0, Vec::new()).unwrap()).is_none()
    );
    assert!(factor_residuals(&returns, &factors(4, 40)).is_none(), "more factors than assets");
    assert!(factor_residuals(&returns, &factors(1, 39)).is_none(), "the rows differ");
    let short = columns(&[noise(1, 3), noise(2, 3), noise(3, 3)]);
    assert!(
        factor_residuals(&short, &factors(2, 3)).is_none(),
        "no rows left to fit a factor model on"
    );
}

/// `assets` series that each follow `factors[group]` plus their own noise, `groups[i]` naming asset `i`'s.
fn factor_driven(groups: &[usize], factors: &[Vec<f64>], idiosyncratic: f64, seed: u64) -> Matrix {
    let n = factors[0].len();
    let series: Vec<Vec<f64>> = groups
        .iter()
        .enumerate()
        .map(|(i, &group)| {
            let own = noise(seed + i as u64, n);
            (0..n).map(|t| factors[group][t] + idiosyncratic * own[t]).collect()
        })
        .collect();
    columns(&series)
}

/// Six assets in two blocks of three, each block driven by its own factor: both clusterings return the blocks.
#[test]
fn both_clusterings_find_two_blocks_of_assets() {
    let factors = [noise(1, 300), noise(2, 300)];
    let returns = factor_driven(&[0, 0, 0, 1, 1, 1], &factors, 0.3, 10);
    let blocks = vec![vec![0, 1, 2], vec![3, 4, 5]];
    for method in [Clustering::SingleLinkage, Clustering::KMeans] {
        assert_eq!(cluster_returns(&returns, 4, method).unwrap(), blocks, "{method:?}");
    }
}

#[test]
fn clustering_has_no_answer_without_a_correlation_or_an_admissible_count() {
    let flat = Matrix::from_row_major(4, 3, vec![1.0; 12]).unwrap();
    assert!(
        cluster_returns(&flat, 2, Clustering::SingleLinkage).is_none(),
        "returns that never move"
    );
    let two = columns(&[noise(1, 50), noise(2, 50)]);
    assert!(
        cluster_returns(&two, 3, Clustering::KMeans).is_none(),
        "a silhouette needs three assets"
    );
}
