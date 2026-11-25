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

/// Assets 0 and 1 at `ρ = 0.9` (variance 1 each), asset 2 alone (variance 4). The pair merges first
/// (cluster 3), then asset 2 joins it as the lower id: the order is `[2, 0, 1]`. The bisection cuts
/// `[2] | [0, 1]`: `V([2]) = 4`, `V([0, 1]) = ¼ (1 + 1 + 1.8) = 0.95`, so asset 2 gets
/// `1 − 4 / 4.95 = 19/99` and the pair `80/99`, split evenly: `40/99` each.
#[test]
fn hierarchical_risk_parity_of_three_assets_by_hand() {
    let covariance = matrix(3, &[1.0, 0.9, 0.0, 0.9, 1.0, 0.0, 0.0, 0.0, 4.0]);
    let correlation = correlation_from_covariance(&covariance).unwrap();
    assert_eq!(quasi_diagonal_order(&correlation).unwrap(), vec![2, 0, 1]);
    let weights = hierarchical_risk_parity(&covariance).unwrap();
    close(&weights, &[40.0 / 99.0, 40.0 / 99.0, 19.0 / 99.0], 1e-15);
}

/// Two pairs, `ρ₀₂ = 0.9` and `ρ₁₃ = 0.8`, unit variances, nothing across. `d̃₀₂ = √0.1` and
/// `d̃₁₃ = √0.2` merge first (clusters 4 and 5), so the order is `[0, 2, 1, 3]` and the first cut is
/// pair against pair: `V = 0.95` and `0.9`, so the first pair gets `0.9 / 1.85` (`9/37` each) and the
/// second `0.95 / 1.85` (`19/74` each).
#[test]
fn hierarchical_risk_parity_of_two_pairs_by_hand() {
    let covariance = matrix(
        4,
        &[1.0, 0.0, 0.9, 0.0, 0.0, 1.0, 0.0, 0.8, 0.9, 0.0, 1.0, 0.0, 0.0, 0.8, 0.0, 1.0],
    );
    assert_eq!(quasi_diagonal_order(&covariance).unwrap(), vec![0, 2, 1, 3]);
    let weights = hierarchical_risk_parity(&covariance).unwrap();
    close(&weights, &[9.0 / 37.0, 19.0 / 74.0, 9.0 / 37.0, 19.0 / 74.0], 1e-15);
    // Each asset's silhouette at k = 2 is about 0.73 or 0.62; at k = 3 two of them are lone (0), so
    // the quality (mean over spread) is about 12 against 1: the two pairs.
    assert_eq!(clusters_by_silhouette(&covariance, 3).unwrap(), vec![vec![0, 2], vec![1, 3]]);
}

#[test]
fn no_answer_without_a_correlation_or_an_admissible_cut() {
    assert!(hierarchical_risk_parity(&matrix(2, &[1.0, 0.0, 0.0, 0.0])).is_none());
    assert!(hierarchical_risk_parity(&matrix(2, &[1.0, f64::NAN, 0.0, 1.0])).is_none());
    let pair = matrix(2, &[1.0, 0.5, 0.5, 1.0]);
    assert!(clusters_by_silhouette(&pair, 5).is_none(), "a silhouette needs three assets");
    let three = matrix(3, &[1.0, 0.9, 0.0, 0.9, 1.0, 0.0, 0.0, 0.0, 1.0]);
    assert!(clusters_by_silhouette(&three, 1).is_none());
    assert_eq!(clusters_by_silhouette(&three, 9).unwrap(), vec![vec![0, 1], vec![2]]);
    // One asset is its own order and holds everything.
    assert_eq!(hierarchical_risk_parity(&matrix(1, &[2.0])).unwrap(), vec![1.0]);
}
