//! Proves [`GbdtLearner`] satisfies this crate's own learner seam — nothing about model quality.
//!
//! Double-gated, the same shape as this repo's venue live smokes and
//! `crates/vike-ml-training/tests/lightgbm_cli_smoke.rs`: `#[ignore]`d so no ordinary run touches them, AND
//! self-skipping when the trainer binary is absent, so an explicit `--ignored` run on a machine
//! without one prints a reason instead of failing.
//!
//! ```text
//! cargo test -p vike-ml-training --test integration -- --ignored --nocapture gbdt_learner_smoke::
//! ```
//!
//! ⚠ Every assertion here goes through the seam and only the seam. The helpers below are GENERIC
//! over [`Learner`] on purpose: inside a generic function `m.predict_one(..)` can only resolve to
//! [`Predictor::predict_one`], whereas a concrete `GbdtModel` has INHERENT methods (`predict_proba`,
//! `predict_into`) that win method resolution — so a concrete test would pass with the trait impl
//! deleted. That is also why this file names no concrete model type.
//!
//! ⚠ No environment variable decides where the binary is. The location is a CONVENTION
//! (`bin/lightgbm/lightgbm`, relative to the repo root) resolved from `CARGO_MANIFEST_DIR`,
//! which is a compile-time macro and not a process-environment read.

use std::path::PathBuf;

use vike_paths::scratch::ScratchDir;

use vike_ml_prediction::Predictor;
use vike_ml_training::{DEFAULT_POINT, GbdtLearner, GbdtParams, GridPoint, Learner, TrainData};

/// `<repo>/bin/lightgbm/lightgbm` — the path `just lightgbm-build` writes by default.
fn binary_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../bin/lightgbm/lightgbm")
}

/// `Some(learner)` when the pinned binary is present; `None`, with a printed reason, otherwise.
/// ⚠ Returns the [`ScratchDir`] ALONGSIDE the learner, and the caller must bind it — dropping it
/// removes the scratch root the learner trains in.
fn learner(tag: &str) -> Option<(ScratchDir, GbdtLearner)> {
    learner_with(tag, GbdtParams::default())
}

/// [`learner`] over `params` (another objective).
fn learner_with(tag: &str, params: GbdtParams) -> Option<(ScratchDir, GbdtLearner)> {
    let binary = binary_path();
    if !binary.exists() {
        eprintln!("SKIP: no LightGBM binary at {} — run `just lightgbm-build`", binary.display());
        return None;
    }
    let scratch =
        ScratchDir::create_in(&std::env::temp_dir(), &format!("vike_ml_gbdt_smoke_{tag}"))
            .expect("scratch dir");
    let l = GbdtLearner::new(&binary, &scratch, params)
        .expect("a present binary must be the pinned version");
    Some((scratch, l))
}

/// The fallback point — `vike_ml_training::DEFAULT_POINT`, imported rather than copied.
///
/// ⚠ Deliberately NOT the point that discriminates a dropped axis mapping: its
/// `min_data_in_leaf` (20) is also `GbdtParams::default()`'s. That job belongs to
/// `crates/vike-ml-training/src/train/gbdt_learner_tests.rs`'s `every_searched_axis_reaches_the_parameter_it_names`,
/// which needs no binary. THIS file proves the seam end-to-end against the real binary, at the
/// point a caller actually falls back to.
fn point() -> GridPoint {
    DEFAULT_POINT
}

/// 300 rows, 4 features, the last a small-cardinality category (a pooled fit with the asset code
/// as the trailing categorical column).
///
/// The label is NOT a clean function of one feature: a perfectly separable problem is fitted
/// identically under every seed, which would make `two_fits_with_different_seeds_differ` pass or
/// fail on the fixture rather than on the seed.
fn fixture() -> (Vec<f64>, Vec<f32>, usize, usize) {
    let (n, cols) = (300usize, 4usize);
    let mut x = Vec::with_capacity(n * cols);
    let mut y = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f64;
        let a = (t * 0.37).sin();
        let b = (t * 0.11).cos();
        let c = (t * 0.71).sin() * 0.5;
        let cat = (i % 5) as f64;
        x.extend_from_slice(&[a, b, c, cat]);
        let bump = if cat as u32 == 2 { 0.6 } else { -0.2 };
        y.push(if a + 0.5 * b + c + bump > 0.0 { 1.0 } else { 0.0 });
    }
    (x, y, n, cols)
}

/// Fit and read one probability per row — through the seam, and only through the seam.
fn probabilities<L: Learner>(l: &L, d: &TrainData<'_>, p: &GridPoint, seed: u64) -> Vec<f64> {
    let m = l.fit(d, p, seed).expect("fit");
    (0..d.n_rows).map(|i| m.predict_one(d.row(i)).expect("one probability per row")).collect()
}

#[test]
#[ignore = "needs the pinned LightGBM binary — run `just lightgbm-build`"]
fn a_trained_model_returns_a_probability_in_the_unit_interval_for_every_row() {
    let Some((_scratch, l)) = learner("unit") else { return };
    let (x, y, n, cols) = fixture();
    let d = TrainData { x: &x, n_rows: n, n_cols: cols, y: &y, categorical: &[cols - 1] };
    let ps = probabilities(&l, &d, &point(), 7);
    assert_eq!(ps.len(), n);
    for (i, p) in ps.iter().enumerate() {
        // ⚠ This rejects NaN on purpose (`contains` is false for NaN): the learner does not
        // sanitise one, so this is the assertion that would notice a real fit producing it.
        assert!((0.0..=1.0).contains(p), "row {i}: {p}");
    }
    // A constant answer is what a fit that silently trained zero useful trees looks like.
    let first = ps[0];
    assert!(
        ps.iter().any(|p| *p != first),
        "every row got the same probability ({first}) — the fit separated nothing"
    );
}

#[test]
#[ignore = "needs the pinned LightGBM binary — run `just lightgbm-build`"]
fn two_fits_with_the_same_seed_predict_identically() {
    // Same fold, point and seed must give the same model bit for bit, not to a tolerance.
    let Some((_scratch, l)) = learner("same_seed") else { return };
    let (x, y, n, cols) = fixture();
    let d = TrainData { x: &x, n_rows: n, n_cols: cols, y: &y, categorical: &[cols - 1] };
    let a = probabilities(&l, &d, &point(), 11);
    let b = probabilities(&l, &d, &point(), 11);
    assert_eq!(a.len(), b.len());
    for (i, (p, q)) in a.iter().zip(&b).enumerate() {
        assert_eq!(p.to_bits(), q.to_bits(), "row {i}: {p} vs {q}");
    }
}

#[test]
#[ignore = "needs the pinned LightGBM binary — run `just lightgbm-build`"]
fn two_fits_with_different_seeds_differ() {
    // Without this, the test above passes vacuously against a learner that never wired the seed
    // through. With a `GbdtParams::default()` base (no row bagging) the seed reaches the model
    // through the point's `feature_fraction` (0.7) sampling.
    let Some((_scratch, l)) = learner("diff_seed") else { return };
    let (x, y, n, cols) = fixture();
    let d = TrainData { x: &x, n_rows: n, n_cols: cols, y: &y, categorical: &[cols - 1] };
    let a = probabilities(&l, &d, &point(), 11);
    let b = probabilities(&l, &d, &point(), 12);
    assert!(
        a.iter().zip(&b).any(|(p, q)| p.to_bits() != q.to_bits()),
        "two seeds produced bit-identical predictions on every one of {n} rows"
    );
}

#[test]
#[ignore = "needs the pinned LightGBM binary — run `just lightgbm-build`"]
fn a_grid_point_changes_the_model_the_seam_hands_back() {
    // A learner could satisfy every other assertion here while ignoring the point entirely.
    let Some((_scratch, l)) = learner("point") else { return };
    let (x, y, n, cols) = fixture();
    let d = TrainData { x: &x, n_rows: n, n_cols: cols, y: &y, categorical: &[cols - 1] };
    let small = probabilities(&l, &d, &point(), 11);
    let big = probabilities(
        &l,
        &d,
        &GridPoint {
            num_leaves: 31,
            max_depth: 5,
            min_data_in_leaf: 50,
            learning_rate: 0.1,
            feature_fraction: 0.9,
        },
        11,
    );
    assert!(
        small.iter().zip(&big).any(|(p, q)| p.to_bits() != q.to_bits()),
        "two very different grid points produced bit-identical predictions"
    );
}

#[test]
#[ignore = "needs the pinned LightGBM binary — run `just lightgbm-build`"]
fn concurrent_fits_do_not_train_each_others_parameters() {
    // `fit` is called on one `&self` from many threads; two fits sharing a scratch path would
    // silently train each other's parameters. Each thread re-runs a fit whose answer is known.
    let Some((_scratch, l)) = learner("concurrent") else { return };
    let (x, y, n, cols) = fixture();
    let d = TrainData { x: &x, n_rows: n, n_cols: cols, y: &y, categorical: &[cols - 1] };
    let want = probabilities(&l, &d, &point(), 11);
    std::thread::scope(|s| {
        for _ in 0..8 {
            s.spawn(|| {
                let got = probabilities(&l, &d, &point(), 11);
                assert_eq!(got.len(), want.len());
                for (i, (p, q)) in got.iter().zip(&want).enumerate() {
                    assert_eq!(p.to_bits(), q.to_bits(), "row {i}");
                }
            });
        }
    });
}

/// LightGBM `regression` as BOTH nuisances of `vike_ml_training::double_ml::partially_linear`,
/// which takes any learner through the seam. One confounder `x` uniform on [−2, 2) drives both:
/// `D = sin(1.5x) + v` and `Y = θD + 2 sin(1.5x) + 0.5x² + ε` (`v`, `ε` uniform, variance 1/4 and
/// 1/12), θ = 0.5. The naive slope of `Y` on `D` converges to about θ + 1.35; the cross-fitted
/// estimate's 99% interval must cover θ and exclude the naive slope.
#[test]
#[ignore = "needs the pinned LightGBM binary — run `just lightgbm-build`"]
fn a_regression_model_is_a_double_ml_nuisance() {
    use vike_ml_training::double_ml::{CrossFitting, DoubleMlData, partially_linear};
    use vike_model::test_support::corpus::lcg;

    const THETA: f64 = 0.5;
    let params = GbdtParams { objective: "regression".to_string(), ..GbdtParams::default() };
    let Some((_scratch, l)) = learner_with("double_ml", params) else { return };
    let (rows, mut s) = (2_000usize, 17u64);
    let (mut x, mut d, mut y) = (Vec::new(), Vec::new(), Vec::new());
    for _ in 0..rows {
        let confounder = 4.0 * lcg(&mut s) - 2.0;
        let wave = libm::sin(1.5 * confounder);
        let treatment = wave + 3f64.sqrt() * (lcg(&mut s) - 0.5);
        let noise = lcg(&mut s) - 0.5;
        y.push(THETA * treatment + 2.0 * wave + 0.5 * confounder * confounder + noise);
        d.push(treatment);
        x.push(confounder);
    }
    let folds: Vec<(Vec<usize>, Vec<usize>)> =
        (0..5).map(|k| (0..rows).partition(|i| i % 5 != k)).collect();
    let data = DoubleMlData { confounders: &x, column_count: 1, treatment: &d, outcome: &y };
    let grid_point = point();
    let cross = CrossFitting { folds: &folds, point: &grid_point, seed: 7 };
    let e = partially_linear(&data, &l, &l, &cross).expect("LightGBM nuisances fit");
    let (low, high) = e.confidence_interval(0.99).unwrap();

    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
    let (d_mean, y_mean) = (mean(&d), mean(&y));
    let covariance: f64 = d.iter().zip(&y).map(|(a, b)| (a - d_mean) * (b - y_mean)).sum();
    let variance: f64 = d.iter().map(|a| (a - d_mean) * (a - d_mean)).sum();
    let naive = covariance / variance;
    eprintln!("double ml: {e:?}, 99% interval ({low}, {high}); naive slope {naive}");
    assert!(low <= THETA && THETA <= high, "{e:?}: ({low}, {high}) misses θ = {THETA}");
    assert!(naive > high + 0.5, "the naive slope {naive} is not biased past ({low}, {high})");
}
