use super::*;

fn matrix(n: usize, values: &[f64]) -> Matrix {
    Matrix::from_row_major(n, n, values.to_vec()).unwrap()
}

fn close(got: &[f64], want: &[f64], tolerance: f64) {
    assert_eq!(got.len(), want.len(), "{got:?} vs {want:?}");
    for (g, w) in got.iter().zip(want) {
        assert!((g - w).abs() <= tolerance, "{got:?} vs {want:?}");
    }
}

/// Every asset its own cluster: each holds all of itself, the reduced covariance IS the covariance,
/// and the answer is the unnested one bit for bit.
#[test]
fn singleton_clusters_are_the_unnested_portfolio() {
    let covariance = matrix(3, &[0.04, 0.006, -0.004, 0.006, 0.09, 0.009, -0.004, 0.009, 0.16]);
    let singletons = [vec![0], vec![1], vec![2]];
    assert_eq!(
        nested_clustered_optimisation(&covariance, None, &singletons),
        minimum_variance(&covariance)
    );
    let returns: &[f64] = &[0.08, -0.02, 0.05];
    assert_eq!(
        nested_clustered_optimisation(&covariance, Some(returns), &singletons),
        maximum_sharpe(&covariance, returns)
    );
}

/// On a block-diagonal covariance with its blocks as the clusters, nesting changes nothing: the
/// tangency portfolio is `Σ⁻¹μ` block by block, rescaled once. The second block's `1ᵀΣ⁻¹μ` is
/// negative, and it is carried, not refused.
#[test]
fn block_clusters_of_a_block_diagonal_covariance_are_the_unnested_portfolio() {
    let covariance = matrix(
        4,
        &[0.04, 0.01, 0.0, 0.0, 0.01, 0.09, 0.0, 0.0, 0.0, 0.0, 0.16, -0.02, 0.0, 0.0, -0.02, 0.25],
    );
    let blocks = [vec![0, 1], vec![2, 3]];
    let nested = nested_clustered_optimisation(&covariance, None, &blocks).unwrap();
    close(&nested, &minimum_variance(&covariance).unwrap(), 1e-14);
    let returns: &[f64] = &[0.10, 0.05, -0.03, 0.01];
    let nested = nested_clustered_optimisation(&covariance, Some(returns), &blocks).unwrap();
    close(&nested, &maximum_sharpe(&covariance, returns).unwrap(), 1e-14);
    assert!(nested[2] < 0.0, "the argued-against cluster is held short: {nested:?}");
}

#[test]
fn clusters_that_are_not_a_partition_have_no_answer() {
    let covariance = matrix(3, &[1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
    for clusters in [
        vec![vec![0, 1]],
        vec![vec![0, 1], vec![1, 2]],
        vec![vec![0, 1], vec![2, 3]],
        vec![vec![0, 1, 2], vec![]],
    ] {
        assert!(
            nested_clustered_optimisation(&covariance, None, &clusters).is_none(),
            "{clusters:?}"
        );
    }
    let whole = [vec![2, 0, 1]];
    close(
        &nested_clustered_optimisation(&covariance, None, &whole).unwrap(),
        &[1.0 / 3.0; 3],
        1e-15,
    );
    assert!(nested_clustered_optimisation(&covariance, Some(&[0.1, 0.1][..]), &whole).is_none());
}
