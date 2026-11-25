use vike_arbitrage::cointegration::critical_values::{JohansenStatistic, Significance};
use vike_arbitrage::cointegration::{JohansenCase, engle_granger, johansen};
use vike_math::linear_algebra::Matrix;
use vike_math::stationarity_tests::{self, Deterministic};

use crate::common::{noise, walk};

fn near(got: f64, want: f64, tolerance: f64) -> bool {
    (got - want).abs() <= tolerance * want.abs().max(1.0)
}

/// `y = intercept + slope · x + e`, with `e` a first-order autoregression, so `y` and `x` cointegrate.
fn cointegrated(x: &[f64], intercept: f64, slope: f64, seed: u64) -> Vec<f64> {
    let mut e = 0.0;
    noise(seed, x.len())
        .into_iter()
        .zip(x)
        .map(|(u, x)| {
            e = 0.5 * e + 0.3 * u;
            intercept + slope * x + e
        })
        .collect()
}

/// The series as the columns of one matrix, one row per observation.
fn columns<T: AsRef<[f64]>>(series: &[T]) -> Matrix {
    let (n, k) = (series[0].as_ref().len(), series.len());
    let values: Vec<f64> =
        (0..n).flat_map(|t| series.iter().map(move |column| column.as_ref()[t])).collect();
    Matrix::from_row_major(n, k, values).unwrap()
}

/// By hand: `y = [2, 4, 5, 8, 9]` on `x = [1, 2, 3, 4, 5]` has `x̄ = 3`, `ȳ = 5.6`, `Sxx = 10`, `Sxy = 18`,
/// so the slope is 1.8 and the intercept `5.6 − 5.4 = 0.2`; the residuals are `[0, 0.2, −0.6, 0.6, −0.2]`
/// and the statistic is the Dickey–Fuller statistic of those with no deterministic term.
#[test]
fn engle_granger_regression_and_statistic_by_hand() {
    let x = Matrix::from_row_major(5, 1, vec![1.0, 2.0, 3.0, 4.0, 5.0]).unwrap();
    let test = engle_granger(&[2.0, 4.0, 5.0, 8.0, 9.0], &x, 0).unwrap();
    assert!(near(test.coefficients[0], 0.2, 1e-12), "{:?}", test.coefficients);
    assert!(near(test.coefficients[1], 1.8, 1e-12), "{:?}", test.coefficients);
    for (got, want) in test.residuals.iter().zip([0.0, 0.2, -0.6, 0.6, -0.2]) {
        assert!((got - want).abs() < 1e-12, "{:?}", test.residuals);
    }
    let statistic =
        stationarity_tests::augmented_dickey_fuller(&test.residuals, 0, Deterministic::Absent);
    assert_eq!(test.statistic.to_bits(), statistic.to_bits());
    assert_eq!(test.observations, 4);
    assert_eq!(test.lags, 0);
}

/// The residual of a least-squares fit with an intercept is orthogonal to the intercept and to every regressor.
#[test]
fn the_residual_is_orthogonal_to_the_design() {
    let x = walk(1, 300);
    let z = walk(2, 300);
    let y = cointegrated(&x, 1.0, 2.0, 3);
    let test = engle_granger(&y, &columns(&[&x, &z]), 1).unwrap();
    let (mut sum, mut with_x, mut with_z) = (0.0, 0.0, 0.0);
    for ((u, xt), zt) in test.residuals.iter().zip(&x).zip(&z) {
        sum += u;
        with_x += u * xt;
        with_z += u * zt;
    }
    assert!(
        sum.abs() < 1e-9 && with_x.abs() < 1e-8 && with_z.abs() < 1e-8,
        "{sum} {with_x} {with_z}"
    );
    assert_eq!(test.coefficients.len(), 3);
}

/// `y = 1.5 + 2x + e` with a stationary `e` is cointegrated: the slope is found and the statistic is far
/// below the 1% critical value. Two independent walks are not: their statistics stay above it.
#[test]
fn engle_granger_finds_a_cointegrated_pair_and_not_independent_walks() {
    let x = walk(1, 400);
    let y = cointegrated(&x, 1.5, 2.0, 2);
    let test = engle_granger(&y, &columns(&[&x]), 1).unwrap();
    assert!((test.coefficients[1] - 2.0).abs() < 0.02, "{:?}", test.coefficients);
    assert!(test.cointegrated(Significance::OnePercent), "{test:?}");
    assert!(test.statistic < -6.0, "{}", test.statistic);

    for (seed_a, seed_b) in [(3, 4), (5, 6)] {
        let (a, b) = (walk(seed_a, 400), walk(seed_b, 400));
        let test = engle_granger(&a, &columns(&[&b]), 1).unwrap();
        assert!(!test.cointegrated(Significance::OnePercent), "{seed_a} {seed_b}: {test:?}");
    }
}

#[test]
fn engle_granger_has_no_answer_for_a_bad_shape() {
    let x = Matrix::from_row_major(5, 1, vec![1.0, 2.0, 3.0, 4.0, 5.0]).unwrap();
    assert!(engle_granger(&[1.0, 2.0, 3.0], &x, 0).is_none(), "the rows disagree");
    assert!(engle_granger(&[2.0, 4.0, 5.0, 8.0, f64::NAN], &x, 0).is_none());
    let none = Matrix::from_row_major(5, 0, Vec::new()).unwrap();
    assert!(engle_granger(&[2.0, 4.0, 5.0, 8.0, 9.0], &none, 0).is_none());
    let flat = Matrix::from_row_major(5, 1, vec![1.0; 5]).unwrap();
    assert!(engle_granger(&[2.0, 4.0, 5.0, 8.0, 9.0], &flat, 0).is_none(), "a constant regressor");
    assert!(engle_granger(&[2.0, 4.0, 5.0, 8.0, 9.0], &x, 5).is_none(), "no degree of freedom");
}

/// The Dickey–Fuller regression of one series with no deterministic term and no lag is the squared
/// canonical correlation between `Δy_t` and `y_(t-1)`: by hand, `y = [1, 3, 2, 5, 4, 6]` gives
/// `Σ y_(t-1) Δy_t = 8`, `Σ y_(t-1)² = 55`, `Σ Δy_t² = 19`, so `λ = 64 / (55 · 19) = 64 / 1045` and the
/// trace statistic of rank zero is `−5 ln(1 − 64/1045)` over the five rows. The one cointegrating
/// "vector" is the series itself, `1` after scaling.
#[test]
fn johansen_of_one_series_by_hand() {
    let levels = Matrix::from_row_major(6, 1, vec![1.0, 3.0, 2.0, 5.0, 4.0, 6.0]).unwrap();
    let test = johansen(&levels, 0, JohansenCase::NoConstant).unwrap();
    assert_eq!(test.observations, 5);
    assert!(near(test.eigenvalues[0], 64.0 / 1045.0, 1e-12), "{:?}", test.eigenvalues);
    let want = -5.0 * libm::log(981.0 / 1045.0);
    assert!(near(test.trace_statistics[0], want, 1e-12), "{:?}", test.trace_statistics);
    assert_eq!(test.trace_statistics[0].to_bits(), test.maximum_eigenvalue_statistics[0].to_bits());
    assert_eq!(test.rank(JohansenStatistic::Trace, Significance::FivePercent), 0);
    assert!(near(test.spread_weights(0).unwrap()[0], 1.0, 1e-12));
    assert!(test.spread_weights(1).is_none());
}

/// The canonical correlations do not move when the series are replaced by independent linear
/// combinations of themselves (`X → X A`, `A` invertible): the test is about the space the series span.
#[test]
fn johansen_eigenvalues_are_invariant_to_an_invertible_transformation() {
    let (a, b, c) = (walk(11, 250), walk(12, 250), walk(13, 250));
    let original = columns(&[&a, &b, &c]);
    let first: Vec<f64> = (0..250).map(|t| a[t] + 3.0 * b[t] - 2.0 * c[t]).collect();
    let second: Vec<f64> = (0..250).map(|t| b[t] + 0.5 * c[t]).collect();
    let mixed = columns(&[&first, &second, &c]);
    for case in [
        JohansenCase::NoConstant,
        JohansenCase::RestrictedConstant,
        JohansenCase::UnrestrictedConstant,
    ] {
        let before = johansen(&original, 2, case).unwrap();
        let after = johansen(&mixed, 2, case).unwrap();
        for (x, y) in before.eigenvalues.iter().zip(&after.eigenvalues) {
            assert!(near(*x, *y, 1e-8), "{case:?}: {x} vs {y}");
        }
    }
}

/// The trace statistic of rank `r` is the sum of the maximal-eigenvalue terms from `r` on, the
/// eigenvalues come largest first inside `[0, 1)`, and so the statistics fall as `r` grows.
#[test]
fn johansen_statistics_are_consistent() {
    let (a, b) = (walk(21, 300), walk(22, 300));
    let y = cointegrated(&a, 0.0, 1.0, 23);
    let test = johansen(&columns(&[&y, &a, &b]), 1, JohansenCase::UnrestrictedConstant).unwrap();
    assert_eq!(test.eigenvalues.len(), 3);
    for pair in test.eigenvalues.windows(2) {
        assert!(pair[0] >= pair[1], "{:?}", test.eigenvalues);
    }
    assert!(test.eigenvalues.iter().all(|l| (0.0..1.0).contains(l)), "{:?}", test.eigenvalues);
    for r in 0..3 {
        let mut total = 0.0;
        for term in &test.maximum_eigenvalue_statistics[r..] {
            total += term;
        }
        assert!(near(test.trace_statistics[r], total, 1e-12), "{r}");
    }
    for pair in test.trace_statistics.windows(2) {
        assert!(pair[0] >= pair[1], "{:?}", test.trace_statistics);
    }
    assert_eq!(test.trace_critical_values.len(), 3);
}

/// `y = 1.5 + 2x + e` has one cointegrating relation, whose vector is `(1, −2)` after scaling (and the
/// constant `−1.5` in the restricted case); independent walks have none.
#[test]
fn johansen_finds_one_relation_and_its_hedge_ratio() {
    let x = walk(1, 400);
    let y = cointegrated(&x, 1.5, 2.0, 2);
    let pair = columns(&[&y, &x]);
    for case in [JohansenCase::RestrictedConstant, JohansenCase::UnrestrictedConstant] {
        let test = johansen(&pair, 1, case).unwrap();
        assert_eq!(test.rank(JohansenStatistic::Trace, Significance::FivePercent), 1, "{case:?}");
        assert_eq!(
            test.rank(JohansenStatistic::MaximumEigenvalue, Significance::FivePercent),
            1,
            "{case:?}"
        );
        let weights = test.spread_weights(0).unwrap();
        assert!(near(weights[0], 1.0, 1e-12));
        assert!((weights[1] + 2.0).abs() < 0.05, "{case:?}: {weights:?}");
        if case == JohansenCase::RestrictedConstant {
            assert_eq!(weights.len(), 3);
            assert!((weights[2] + 1.5).abs() < 0.3, "{weights:?}");
        } else {
            assert_eq!(weights.len(), 2);
        }
    }
    let (a, b) = (walk(3, 400), walk(4, 400));
    let test = johansen(&columns(&[&a, &b]), 1, JohansenCase::UnrestrictedConstant).unwrap();
    assert_eq!(test.rank(JohansenStatistic::Trace, Significance::FivePercent), 0, "{test:?}");
}

#[test]
fn johansen_has_no_answer_for_a_bad_input() {
    // Two rows leave one observation, which no regression can be fitted on.
    let short = Matrix::from_row_major(2, 1, vec![1.0, 2.0]).unwrap();
    assert!(johansen(&short, 0, JohansenCase::NoConstant).is_none());
    let flat = Matrix::from_row_major(30, 1, vec![2.0; 30]).unwrap();
    assert!(johansen(&flat, 0, JohansenCase::NoConstant).is_none(), "a constant series");
    let mut values = walk(1, 30);
    values[7] = f64::NAN;
    assert!(johansen(&columns(&[&values]), 0, JohansenCase::NoConstant).is_none());
    let wide: Vec<Vec<f64>> = (0..13).map(|i| walk(100 + i, 60)).collect();
    let refs: Vec<&[f64]> = wide.iter().map(Vec::as_slice).collect();
    assert!(johansen(&columns(&refs), 0, JohansenCase::NoConstant).is_none(), "13 series");
}

type Pair = [[f64; 2]; 2];

fn product(a: Pair, b: Pair) -> Pair {
    let mut c = [[0.0; 2]; 2];
    for i in 0..2 {
        for j in 0..2 {
            c[i][j] = a[i][0] * b[0][j] + a[i][1] * b[1][j];
        }
    }
    c
}

fn inverse(a: Pair) -> Pair {
    let det = a[0][0] * a[1][1] - a[0][1] * a[1][0];
    [[a[1][1] / det, -a[0][1] / det], [-a[1][0] / det, a[0][0] / det]]
}

/// The textbook computation on the simplest model (no constant, no lagged difference), written out for two series
/// with no library routine: the moment matrices of `ΔX_t` and `X_(t-1)`, the matrix `S₁₁⁻¹ S₁₀ S₀₀⁻¹ S₀₁`, and its two
/// eigenvalues as the roots of `λ² − tr λ + det`. The crate reaches the same numbers through the symmetric
/// square-root form, so this is an independent check of that route.
#[test]
fn johansen_eigenvalues_by_the_textbook_formula() {
    let (a, b) = (walk(31, 80), walk(32, 80));
    let n = 79.0;
    let (mut s00, mut s01, mut s11) = ([[0.0; 2]; 2], [[0.0; 2]; 2], [[0.0; 2]; 2]);
    for t in 1..80 {
        let delta = [a[t] - a[t - 1], b[t] - b[t - 1]];
        let level = [a[t - 1], b[t - 1]];
        for i in 0..2 {
            for j in 0..2 {
                s00[i][j] += delta[i] * delta[j] / n;
                s01[i][j] += delta[i] * level[j] / n;
                s11[i][j] += level[i] * level[j] / n;
            }
        }
    }
    let s10 = [[s01[0][0], s01[1][0]], [s01[0][1], s01[1][1]]];
    let m = product(inverse(s11), product(s10, product(inverse(s00), s01)));
    let (trace, det) = (m[0][0] + m[1][1], m[0][0] * m[1][1] - m[0][1] * m[1][0]);
    let root = (trace * trace - 4.0 * det).sqrt();
    let want = [(trace + root) / 2.0, (trace - root) / 2.0];

    let test = johansen(&columns(&[&a, &b]), 0, JohansenCase::NoConstant).unwrap();
    assert_eq!(test.observations, 79);
    for (got, want) in test.eigenvalues.iter().zip(want) {
        assert!(near(*got, want, 1e-9), "{:?} vs {want:?}", test.eigenvalues);
    }
    // And the statistics follow from them: `−T ln(1 − λ)`, the trace of rank zero summing both.
    let terms: Vec<f64> = want.iter().map(|l| -n * libm::log(1.0 - l)).collect();
    assert!(near(test.maximum_eigenvalue_statistics[0], terms[0], 1e-9));
    assert!(near(test.trace_statistics[0], terms[0] + terms[1], 1e-9));
    assert!(near(test.trace_statistics[1], terms[1], 1e-9));
}
