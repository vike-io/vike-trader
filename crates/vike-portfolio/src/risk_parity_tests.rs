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

/// `σ = (1, 2)`: inverse volatility `(1, 1/2) / 1.5 = (2/3, 1/3)`, inverse variance
/// `(1, 1/4) / 1.25 = (0.8, 0.2)`.
#[test]
fn the_inverse_weights_by_hand() {
    let covariance = matrix(2, &[1.0, 0.6, 0.6, 4.0]);
    close(&inverse_volatility(&covariance).unwrap(), &[2.0 / 3.0, 1.0 / 3.0], 1e-15);
    close(&inverse_variance(&covariance).unwrap(), &[0.8, 0.2], 1e-15);
    assert!(inverse_volatility(&matrix(2, &[1.0, 0.0, 0.0, 0.0])).is_none());
    assert!(inverse_variance(&matrix(2, &[1.0, 0.0, 0.0, -1.0])).is_none());
}

/// `w = (½, ½)` on `[[1, 0.5], [0.5, 4]]`: `Σw = (0.75, 2.25)`, so the contributions are
/// `(0.375, 1.125)`, summing to the variance `1.5`.
#[test]
fn the_risk_contributions_by_hand() {
    let covariance = matrix(2, &[1.0, 0.5, 0.5, 4.0]);
    close(&risk_contributions(&covariance, &[0.5, 0.5]).unwrap(), &[0.375, 1.125], 0.0);
    assert!(risk_contributions(&covariance, &[1.0]).is_none());
}

/// Two assets at any correlation, and any number at one common correlation, give equal risk
/// contributions at the inverse-volatility weights (Maillard, Roncalli and Teïletche, 2010):
/// `σ = (1, 2, 4)` at `ρ = 0.5` is `(1, ½, ¼) / 1.75 = (4/7, 2/7, 1/7)`.
#[test]
fn risk_parity_is_inverse_volatility_where_the_closed_form_says_so() {
    let pair = matrix(2, &[1.0, 0.6, 0.6, 4.0]);
    close(&risk_parity(&pair).unwrap(), &[2.0 / 3.0, 1.0 / 3.0], 1e-14);
    let sigma = [1.0, 2.0, 4.0];
    let mut values = Vec::new();
    for (i, a) in sigma.iter().enumerate() {
        for (j, b) in sigma.iter().enumerate() {
            values.push(if i == j { a * b } else { 0.5 * a * b });
        }
    }
    close(&risk_parity(&matrix(3, &values)).unwrap(), &[4.0 / 7.0, 2.0 / 7.0, 1.0 / 7.0], 1e-14);
}

/// Unequal correlations: the answer is NOT inverse volatility, and its contributions are equal.
#[test]
fn risk_parity_equalises_the_contributions() {
    let covariance = matrix(3, &[1.0, 0.8, 0.1, 0.8, 1.0, -0.2, 0.1, -0.2, 1.0]);
    let weights = risk_parity(&covariance).unwrap();
    let total = weights.iter().fold(0.0, |t, w| t + w);
    assert!((total - 1.0).abs() < 1e-15, "{weights:?}");
    let contributions = risk_contributions(&covariance, &weights).unwrap();
    let variance = contributions.iter().fold(0.0, |t, c| t + c);
    for c in &contributions {
        assert!((c - variance / 3.0).abs() < 1e-13 * variance, "{contributions:?}");
    }
    // Equal variances, so inverse volatility would be `1/3` each; the correlated pair holds less.
    assert!(weights[0] < 1.0 / 3.0 && weights[2] > 1.0 / 3.0, "{weights:?}");
    assert!(risk_parity(&matrix(2, &[1.0, 0.0, 0.0, 0.0])).is_none());
}
