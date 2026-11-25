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

/// Two assets: `w₁ = (σ₂² − σ₁₂) / (σ₁² + σ₂² − 2σ₁₂) = (4 − 0.5) / (1 + 4 − 1) = 0.875`.
#[test]
fn the_minimum_variance_pair_by_hand() {
    let covariance = matrix(2, &[1.0, 0.5, 0.5, 4.0]);
    close(&minimum_variance(&covariance).unwrap(), &[0.875, 0.125], 1e-15);
}

/// Uncorrelated assets: `wᵢ ∝ μᵢ / σᵢ²`, so `(0.1 / 0.04, 0.05 / 0.09) = (2.5, 5/9)`, i.e. `(9/11, 2/11)`.
#[test]
fn the_maximum_sharpe_pair_by_hand() {
    let covariance = matrix(2, &[0.04, 0.0, 0.0, 0.09]);
    close(&maximum_sharpe(&covariance, &[0.1, 0.05]).unwrap(), &[9.0 / 11.0, 2.0 / 11.0], 1e-15);
    // `1ᵀΣ⁻¹μ < 0`: the normalised vector would be the LEAST Sharpe portfolio.
    assert!(maximum_sharpe(&covariance, &[-0.1, -0.05]).is_none());
    assert!(maximum_sharpe(&covariance, &[0.1]).is_none());
    assert!(maximum_sharpe(&covariance, &[0.1, f64::NAN]).is_none());
}

#[test]
fn no_closed_form_without_a_positive_definite_covariance() {
    assert!(minimum_variance(&matrix(2, &[1.0, 1.0, 1.0, 1.0])).is_none());
    assert!(minimum_variance(&Matrix::from_row_major(1, 2, vec![1.0, 2.0]).unwrap()).is_none());
    assert!(minimum_variance(&matrix(2, &[1.0, f64::NAN, 0.0, 1.0])).is_none());
}

/// `Σ = [[1, 1.8], [1.8, 4]]`: unconstrained `w₁ = 2.2 / 1.4 ≈ 1.571`, a short in the second asset.
/// Long-only the answer is the corner `(1, 0)`: there `Σw = (1, 1.8)`, the held asset's marginal
/// variance is the multiplier `1`, and the empty one's `1.8 ≥ 1`, so no move into it helps (KKT).
#[test]
fn the_long_only_minimum_variance_is_the_constrained_optimum_not_a_clip() {
    let covariance = matrix(2, &[1.0, 1.8, 1.8, 4.0]);
    close(&minimum_variance(&covariance).unwrap(), &[2.2 / 1.4, -0.8 / 1.4], 1e-14);
    close(&long_only_minimum_variance(&covariance).unwrap(), &[1.0, 0.0], 1e-12);
    // Where the unconstrained answer is already long, the two agree.
    let covariance = matrix(2, &[1.0, 0.5, 0.5, 4.0]);
    close(&long_only_minimum_variance(&covariance).unwrap(), &[0.875, 0.125], 1e-11);
}

/// `μ = (0.1, −0.05)` on uncorrelated assets: unconstrained `(9/7, −2/7)`, long-only `(1, 0)` (at
/// `y = (10, 0)`, `Σy = (0.4, 0)`, the multiplier is `0.4 / 0.1 = 4`, and `0 ≥ 4 · (−0.05)`).
#[test]
fn the_long_only_maximum_sharpe_by_hand() {
    let covariance = matrix(2, &[0.04, 0.0, 0.0, 0.09]);
    close(&maximum_sharpe(&covariance, &[0.1, -0.05]).unwrap(), &[9.0 / 7.0, -2.0 / 7.0], 1e-14);
    close(&long_only_maximum_sharpe(&covariance, &[0.1, -0.05]).unwrap(), &[1.0, 0.0], 1e-12);
    close(
        &long_only_maximum_sharpe(&covariance, &[0.1, 0.05]).unwrap(),
        &[9.0 / 11.0, 2.0 / 11.0],
        1e-11,
    );
    assert!(long_only_maximum_sharpe(&covariance, &[-0.1, 0.0]).is_none());
    assert!(long_only_maximum_sharpe(&covariance, &[0.1]).is_none());
}

#[test]
fn the_projection_onto_the_simplex_by_hand() {
    let simplex = [1.0, 1.0, 1.0];
    close(&project_onto_budget(&[2.0, 0.0], &simplex[..2]).unwrap(), &[1.0, 0.0], 0.0);
    close(&project_onto_budget(&[0.5, 0.5], &simplex[..2]).unwrap(), &[0.5, 0.5], 0.0);
    close(&project_onto_budget(&[1.0, 1.0, -5.0], &simplex).unwrap(), &[0.5, 0.5, 0.0], 0.0);
    close(&project_onto_budget(&[0.2, 0.2, 0.2], &simplex).unwrap(), &[1.0 / 3.0; 3], 1e-15);
}

/// `{y ≥ 0, 2y₁ + y₂ = 1}`: the origin projects to `−τ (2, 1)` with `5(−τ) = 1`, so `(0.4, 0.2)`.
/// `{y ≥ 0, y₁ − y₂ = 1}`: the origin projects to `(1, 0)`. No positive budget, no point.
#[test]
fn the_projection_onto_a_weighted_budget_by_hand() {
    close(&project_onto_budget(&[0.0, 0.0], &[2.0, 1.0]).unwrap(), &[0.4, 0.2], 1e-16);
    close(&project_onto_budget(&[0.0, 0.0], &[1.0, -1.0]).unwrap(), &[1.0, 0.0], 0.0);
    // A zero budget leaves its coordinate free: only its sign is projected.
    close(
        &project_onto_budget(&[0.0, 0.7, -0.3], &[1.0, 0.0, 0.0]).unwrap(),
        &[1.0, 0.7, 0.0],
        0.0,
    );
    assert!(project_onto_budget(&[1.0, 1.0], &[0.0, -1.0]).is_none());
}
