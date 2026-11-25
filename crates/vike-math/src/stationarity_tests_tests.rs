use super::*;

/// The relative distance of `got` from `want`.
fn relative(got: f64, want: f64) -> f64 {
    (got - want).abs() / want.abs()
}

/// A fixed uniform sequence in `[-0.5, 0.5)` (no `rand` here: a 64-bit LCG).
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

/// A random walk from 4.6 (a log price near 100).
fn walk(seed: u64, n: usize) -> Vec<f64> {
    let mut level = 4.6;
    noise(seed, n)
        .into_iter()
        .map(|u| {
            level += 0.01 * u;
            level
        })
        .collect()
}

/// Worked by hand from the published regression (no lags, a constant): with `x = y_(t-1)` and
/// `d = Δy_t` centred, `b = Sxd / Sxx`, `s² = SSR / (m - 2)`, `t = b / sqrt(s² / Sxx)`.
/// `y = [1, 3, 2, 5, 4]`: `Sxx = 35/4`, `Sxd = -33/4`, `SSR = 174/35`, so `t = -33 / sqrt(348)`.
#[test]
fn the_dickey_fuller_regression_by_hand() {
    let t = augmented_dickey_fuller(&[1.0, 3.0, 2.0, 5.0, 4.0], 0, Deterministic::Constant);
    assert!(relative(t, -33.0 / 348f64.sqrt()) < 1e-12, "{t}");
    // `y = [1, 3, 2, 5, 4, 6]`: `Sxx = 10`, `Sxd = -7`, `SSR = 9.1` over 3 degrees: `-sqrt(21/13)`.
    let t = augmented_dickey_fuller(&[1.0, 3.0, 2.0, 5.0, 4.0, 6.0], 0, Deterministic::Constant);
    assert!(relative(t, -(21.0f64 / 13.0).sqrt()) < 1e-12, "{t}");
}

/// No deterministic term (the regression on a residual that is centred already): `y = [0, 1, 1, 3]`
/// gives the rows `x = y_(t-1) = [0, 1, 1]` and `d = Δy_t = [1, 0, 2]`, so `b = Sxd / Sxx = 2 / 2 = 1`,
/// `SSR = 3` over 2 degrees and `t = 1 / sqrt(0.75) = 2 / sqrt(3)`.
#[test]
fn the_regression_with_no_deterministic_term_by_hand() {
    let t = augmented_dickey_fuller(&[0.0, 1.0, 1.0, 3.0], 0, Deterministic::Absent);
    assert!(relative(t, 2.0 / 3f64.sqrt()) < 1e-12, "{t}");
}

/// The same six levels with a trend: partialling the centred index `[-2, -1, 0, 1, 2]` out of
/// `x - x̄` leaves `[-0.4, 0.8, -1, 1.2, -0.6]` (and `d - d̄` untouched), so `b = -7 / 3.6`,
/// `SSR = 7/18` over 2 degrees and `t = -sqrt(70)`.
#[test]
fn the_trend_regression_by_hand() {
    let t = augmented_dickey_fuller(
        &[1.0, 3.0, 2.0, 5.0, 4.0, 6.0],
        0,
        Deterministic::ConstantAndTrend,
    );
    assert!(relative(t, -70f64.sqrt()) < 1e-12, "{t}");
}

/// One lagged difference: `y = [1, 3, 2, 5, 4, 6, 5]` gives five rows; partialling the centred lag
/// `[1, -2, 2, -2, 1]` out leaves `x = [-1.5, -1, 0, 1, 1.5]` and `d = [-0.4, 0.6, 0.6, -0.4, -0.4]`,
/// so `b = -2/13`, `SSR = 68/65` over 2 degrees and `t = -sqrt(5/17)`.
#[test]
fn the_augmented_regression_by_hand() {
    let t =
        augmented_dickey_fuller(&[1.0, 3.0, 2.0, 5.0, 4.0, 6.0, 5.0], 1, Deterministic::Constant);
    assert!(relative(t, -(5.0f64 / 17.0).sqrt()) < 1e-12, "{t}");
}

/// The statistic does not move when the levels are rescaled and shifted, and with a trend it does not
/// move when a straight line is added either.
#[test]
fn affine_and_trend_invariance() {
    for seed in [1, 2, 3] {
        let y = walk(seed, 300);
        for lags in [0, 1, 4] {
            for deterministic in [Deterministic::Constant, Deterministic::ConstantAndTrend] {
                let base = augmented_dickey_fuller(&y, lags, deterministic);
                let scaled: Vec<f64> = y.iter().map(|v| 250.0 * v - 7.0).collect();
                let moved = augmented_dickey_fuller(&scaled, lags, deterministic);
                assert!(relative(moved, base) < 1e-9, "{seed} {lags} {deterministic:?}");
            }
            let base = augmented_dickey_fuller(&y, lags, Deterministic::ConstantAndTrend);
            let tilted: Vec<f64> =
                y.iter().enumerate().map(|(i, v)| v + 0.003 * i as f64).collect();
            let moved = augmented_dickey_fuller(&tilted, lags, Deterministic::ConstantAndTrend);
            assert!(relative(moved, base) < 1e-9, "{seed} {lags} trend");
        }
    }
}

/// A strongly mean-reverting series rejects the unit root by far; a random walk does not come near.
#[test]
fn mean_reversion_rejects_and_a_walk_does_not() {
    let mut level = 0.0;
    let reverting: Vec<f64> = noise(9, 500)
        .into_iter()
        .map(|u| {
            level = 0.2 * level + u;
            level
        })
        .collect();
    assert!(augmented_dickey_fuller(&reverting, 1, Deterministic::Constant) < -10.0);
    assert!(augmented_dickey_fuller(&walk(9, 500), 1, Deterministic::Constant) > -6.0);
}

/// No answer is a `NaN`, never a number: too few rows, a constant window, a non-finite level.
#[test]
fn no_answer_is_nan() {
    // Two rows leave no degree of freedom beside the constant and the slope; three leave one.
    assert!(augmented_dickey_fuller(&[1.0, 2.0, 1.5], 0, Deterministic::Constant).is_nan());
    assert!(augmented_dickey_fuller(&[1.0, 2.0, 1.5, 1.7], 0, Deterministic::Constant).is_finite());
    assert!(
        augmented_dickey_fuller(&[1.0, 2.0, 1.5, 1.7], 0, Deterministic::ConstantAndTrend).is_nan()
    );
    assert!(
        augmented_dickey_fuller(&[1.0, 2.0, 1.5, 1.7, 1.2], 2, Deterministic::Constant).is_nan()
    );
    assert!(augmented_dickey_fuller(&[], 0, Deterministic::Constant).is_nan());
    assert!(augmented_dickey_fuller(&[0.1; 50], 1, Deterministic::Constant).is_nan());
    let mut y = walk(4, 60);
    assert!(augmented_dickey_fuller(&y, 1, Deterministic::Constant).is_finite());
    y[30] = f64::NAN;
    assert!(augmented_dickey_fuller(&y, 1, Deterministic::Constant).is_nan());
    y[30] = f64::INFINITY;
    assert!(augmented_dickey_fuller(&y, 1, Deterministic::Constant).is_nan());
}

#[test]
fn sadf_is_the_supremum_over_the_hand_computed_windows() {
    // levels [0, 1, 3, 2, 5], no lag, a constant: Δy = (1, 2, -1, 3) on y_(t-1) = (0, 1, 3, 2).
    // start 0: b = -2.5 / 5, SSE 7.5, se = sqrt(7.5 / 2 / 5)      -> t = -1/√3.
    // start 1: b = -3 / 2,   SSE 25/6, se = sqrt(25/6 / 1 / 2)    -> t = -0.6√3.
    let levels = [0.0, 1.0, 3.0, 2.0, 5.0];
    let sadf = supremum_augmented_dickey_fuller(&levels, 3, 0, Deterministic::Constant);
    assert!((sadf - (-1.0 / 3.0_f64.sqrt())).abs() < 1e-12, "{sadf}");
    // One window only: levels [0, 1, 3, 2] -> t = -11 / (5√3).
    let shorter = supremum_augmented_dickey_fuller(&levels[..4], 3, 0, Deterministic::Constant);
    assert!((shorter - (-11.0 / (5.0 * 3.0_f64.sqrt()))).abs() < 1e-12, "{shorter}");
    // No window of four levels in three.
    assert!(supremum_augmented_dickey_fuller(&levels[..3], 3, 0, Deterministic::Constant).is_nan());
}

#[test]
fn sadf_is_at_least_every_window_it_ranges_over() {
    let levels: Vec<f64> =
        (0..40).map(|i| f64::from(i % 7) + 0.25 * f64::from(i) + f64::from(i * i) / 50.0).collect();
    let sadf = supremum_augmented_dickey_fuller(&levels, 10, 1, Deterministic::Constant);
    assert!(sadf.is_finite());
    for start in 0..=levels.len() - 11 {
        let window = augmented_dickey_fuller(&levels[start..], 1, Deterministic::Constant);
        assert!(window.is_nan() || window <= sadf);
    }
    // The full-history ADF is the first window, so SADF never sits below it.
    assert!(augmented_dickey_fuller(&levels, 1, Deterministic::Constant) <= sadf);
}

#[test]
fn a_constant_stretch_is_skipped_not_returned() {
    // The last window is all 3s: the regression has no answer there, and the others still do.
    let levels = [1.0, 2.0, 4.0, 3.0, 3.0, 3.0, 3.0, 3.0];
    let sadf = supremum_augmented_dickey_fuller(&levels, 4, 0, Deterministic::Constant);
    assert!(sadf.is_finite());
    assert!(augmented_dickey_fuller(&levels[3..], 0, Deterministic::Constant).is_nan());
}

/// Worked by hand from the paper's (13) and (10): `y = [1, 3, 2, 5, 4]` has `e = [-2, 0, -1, 2, 1]`,
/// `S = [-2, -2, -3, -1, 0]`, `Σ S² = 18` and `Σ e² / T = 2`, so `η = 18 / (25 · 2) = 0.36`. The
/// first autocovariance is 0, so one lag changes nothing; the second is `1 / 5`, weighted
/// `2 · (1 - 2/3)`, so two lags give `s² = 32/15` and `η = 0.3375`.
#[test]
fn kpss_by_hand() {
    let y = [1.0, 3.0, 2.0, 5.0, 4.0];
    let mut scratch = Vec::new();
    assert_eq!(kpss_statistic(&y, 0, &mut scratch), 0.36);
    assert_eq!(kpss_statistic(&y, 1, &mut scratch), 0.36);
    assert!((kpss_statistic(&y, 2, &mut scratch) - 0.3375).abs() < 1e-15);
    // A constant window and a `NaN` have no answer.
    assert!(kpss_statistic(&[2.0; 5], 1, &mut scratch).is_nan());
    assert!(kpss_statistic(&[1.0, f64::NAN, 2.0, 3.0], 1, &mut scratch).is_nan());
}
