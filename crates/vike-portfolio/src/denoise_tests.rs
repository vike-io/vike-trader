use super::*;

fn matrix(n: usize, values: &[f64]) -> Matrix {
    Matrix::from_row_major(n, n, values.to_vec()).unwrap()
}

fn close(got: &Matrix, want: &[f64], tolerance: f64) {
    assert_eq!(got.values().len(), want.len(), "{got:?} vs {want:?}");
    for (g, w) in got.values().iter().zip(want) {
        assert!((g - w).abs() <= tolerance, "{got:?} vs {want:?}");
    }
}

/// `N = 100` assets over `T = 400` observations: `(1 + √¼)² = 2.25`, scaled by `σ²`.
#[test]
fn the_marchenko_pastur_edge_by_hand() {
    assert_eq!(marchenko_pastur_upper_edge(1.0, 400, 100), Some(2.25));
    assert_eq!(marchenko_pastur_upper_edge(0.5, 400, 100), Some(1.125));
    assert!(marchenko_pastur_upper_edge(0.0, 400, 100).is_none());
    assert!(marchenko_pastur_upper_edge(f64::NAN, 400, 100).is_none());
    assert!(marchenko_pastur_upper_edge(1.0, 0, 100).is_none());
}

/// The pair at `ρ = 0.5` beside a lone asset: eigenvalues `1.5` on `(1, 1, 0)/√2`, `1` on `(0, 0, 1)`
/// and `0.5` on `(1, −1, 0)/√2`. Keeping one, the noise pair becomes `0.75` each, so the rebuilt
/// matrix is `[[1.125, 0.375, 0], [0.375, 1.125, 0], [0, 0, 0.75]]` and its correlation `0.375 / 1.125
/// = 1/3`.
#[test]
fn the_constant_residual_denoising_by_hand() {
    let correlation = matrix(3, &[1.0, 0.5, 0.0, 0.5, 1.0, 0.0, 0.0, 0.0, 1.0]);
    let third = 1.0 / 3.0;
    let want = [1.0, third, 0.0, third, 1.0, 0.0, 0.0, 0.0, 1.0];
    let denoised = denoise_correlation(&correlation, 1).unwrap();
    close(&denoised, &want, 1e-14);
    assert_eq!(denoised, denoised.transpose(), "exactly symmetric");
    // The same through a covariance with deviations (2, 3, 1): scaled back entry by entry.
    let covariance = matrix(3, &[4.0, 3.0, 0.0, 3.0, 9.0, 0.0, 0.0, 0.0, 1.0]);
    let want = [4.0, 2.0, 0.0, 2.0, 9.0, 0.0, 0.0, 0.0, 1.0];
    close(&denoise_covariance(&covariance, 1).unwrap(), &want, 1e-13);
}

/// A flat noise spectrum is left as it is; keeping no signal leaves the identity (every eigenvalue
/// becomes the average, 1); keeping all of them rebuilds the input.
#[test]
fn denoising_leaves_a_flat_spectrum_and_its_limits_alone() {
    let equicorrelated = [1.0, 0.4, 0.4, 0.4, 1.0, 0.4, 0.4, 0.4, 1.0];
    let correlation = matrix(3, &equicorrelated);
    close(&denoise_correlation(&correlation, 1).unwrap(), &equicorrelated, 1e-14);
    close(&denoise_correlation(&correlation, 3).unwrap(), &equicorrelated, 1e-14);
    let identity = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
    close(&denoise_correlation(&correlation, 0).unwrap(), &identity, 1e-14);
    assert!(denoise_correlation(&correlation, 4).is_none());
    assert!(denoise_covariance(&matrix(2, &[1.0, 0.0, 0.0, 0.0]), 1).is_none());
}

/// Without the market component `1.5 · (1, 1, 0)(1, 1, 0)ᵀ / 2`, the pair keeps only
/// `0.5 · (1, −1, 0)(1, −1, 0)ᵀ / 2`: a perfect negative correlation once rescaled.
#[test]
fn detoning_by_hand() {
    let correlation = matrix(3, &[1.0, 0.5, 0.0, 0.5, 1.0, 0.0, 0.0, 0.0, 1.0]);
    let want = [1.0, -1.0, 0.0, -1.0, 1.0, 0.0, 0.0, 0.0, 1.0];
    close(&detone(&correlation, 1).unwrap(), &want, 1e-14);
    assert!(detone(&correlation, 3).is_none());
    // `ρ = 1`: the market is everything, and what is left has no variance.
    assert!(detone(&matrix(2, &[1.0, 1.0, 1.0, 1.0]), 1).is_none());
}
