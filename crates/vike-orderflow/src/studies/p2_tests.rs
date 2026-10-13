use super::*;

/// The exact oracle: full-sort interpolated quantile (same rank convention).
fn exact(sample: &[f64], p: f64) -> f64 {
    let mut v = sample.to_vec();
    v.sort_by(f64::total_cmp);
    interp_sorted(&v, p)
}

/// Deterministic LCG uniforms in [0, 1) — no rand dep.
fn uniforms(n: usize, mut seed: u64) -> Vec<f64> {
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        out.push((seed >> 11) as f64 / (1u64 << 53) as f64);
    }
    out
}

// The worked example from Jain & Chlamtac (1985), Table I: 20 observations, p = 0.5;
// the paper's final middle-marker height is 4.44063.
#[test]
fn p2_matches_the_paper_example() {
    let obs = [
        0.02, 0.15, 0.74, 3.39, 0.83, 22.37, 10.15, 15.43, 38.62, 15.92, 34.60, 10.28, 1.47, 0.40,
        0.05, 11.39, 0.27, 0.42, 0.09, 11.37,
    ];
    let mut est = P2Quantile::new(0.5);
    for &x in &obs {
        est.push(x);
    }
    let v = est.value().unwrap();
    assert!((v - 4.44063).abs() < 1e-4, "paper example: got {v}");
    assert_eq!(est.count(), 20);
}

#[test]
fn small_samples_are_exact() {
    let mut est = P2Quantile::new(0.5);
    assert_eq!(est.value(), None);
    est.push(4.0);
    assert_eq!(est.value(), Some(4.0));
    est.push(2.0);
    assert_eq!(est.value(), Some(3.0)); // median of {2,4}
    est.push(6.0);
    assert_eq!(est.value(), Some(4.0)); // median of {2,4,6}
    est.push(0.0);
    assert_eq!(est.value(), Some(3.0)); // {0,2,4,6} → (2+4)/2
}

#[test]
fn p2_close_to_exact_on_uniform_sample() {
    let xs = uniforms(1000, 42);
    for &p in &[0.1, 0.5, 0.9, 0.95] {
        let mut est = P2Quantile::new(p);
        for &x in &xs {
            est.push(x);
        }
        let got = est.value().unwrap();
        let want = exact(&xs, p);
        assert!(
            (got - want).abs() < 0.02,
            "p={p}: P² {got} vs exact {want} (|Δ| = {})",
            (got - want).abs()
        );
    }
}

// Skewed sample (x³ of uniforms): the estimator must still land near the exact
// quantile — a distribution-shape robustness check, looser gate.
#[test]
fn p2_close_to_exact_on_skewed_sample() {
    let xs: Vec<f64> = uniforms(2000, 7).into_iter().map(|u| u * u * u).collect();
    for &p in &[0.5, 0.9] {
        let mut est = P2Quantile::new(p);
        for &x in &xs {
            est.push(x);
        }
        let got = est.value().unwrap();
        let want = exact(&xs, p);
        assert!((got - want).abs() < 0.03, "p={p}: P² {got} vs exact {want}");
    }
}

// Invariants under a mixed stream: markers stay sorted, estimate within [min, max].
#[test]
fn markers_stay_monotone_and_bounded() {
    let xs = uniforms(500, 99);
    let mut est = P2Quantile::new(0.75);
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for &x in &xs {
        est.push(x);
        lo = lo.min(x);
        hi = hi.max(x);
        if est.count() >= 5 {
            for i in 0..4 {
                assert!(est.q[i] <= est.q[i + 1], "markers out of order at {i}");
            }
            let v = est.value().unwrap();
            assert!((lo..=hi).contains(&v));
        }
    }
}

#[test]
fn nan_is_ignored() {
    let mut est = P2Quantile::new(0.5);
    for x in [1.0, f64::NAN, 2.0, 3.0, f64::NAN, 4.0, 5.0, 6.0, 7.0] {
        est.push(x);
    }
    assert_eq!(est.count(), 7);
    let v = est.value().unwrap();
    assert!(v.is_finite());
}
