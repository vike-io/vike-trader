//! The filters (`src/indicators/filters.rs`), each against its published definition written again
//! here from the source the module doc names: Ehlers' Gaussian filter (the paper's one- to four-pole
//! equations, its coefficient tables, the cascade of exponential averages it says it is), Ehlers'
//! early-onset trend (the article's recurrences, its range, `with_zero_history`), the g-h
//! tracking filter (Brookner's (1.2-11), a constant-velocity ramp tracked with no error, a new track
//! after a missing value) and the scalar Kalman filter (Welch and Bishop's two tables, the
//! precision-weighted mean it is when the level does not move, the steady gain of the Riccati
//! equation, a missing measurement as a time update alone). Each row also streams what it computes
//! in batch, survives `reset`, and its `series::*` call gives the same bits.

use vike_indicators::indicators::{EarlyOnsetTrend, GaussianFilter, GhFilter, Kalman};
use vike_indicators::series;

use crate::fast::{assert_close, closes, same, spread_bars};
use crate::options::{assert_lines_same, checked_stream, registry};

const PI: f64 = std::f64::consts::PI;

// ---------------------------------------------------------------- gaussian filter

/// The paper's `alpha` for `poles` and `period`, with `root` the paper's `1.414^(2/N)` term.
fn gaussian_alpha(period: f64, root: f64) -> f64 {
    let beta = (1.0 - libm::cos(2.0 * PI / period)) / (root - 1.0);
    -beta + (beta * beta + 2.0 * beta).sqrt()
}

/// The paper's "Nuts and Bolts" equations for one to four poles, as printed, from 0, each power
/// `libm::pow`'s. The expanded form magnifies a coefficient's last bit by about `alpha^-N` (a
/// million at 400 bars), so a reference with other powers could only be held to a tolerance that
/// size; with the same powers it is bit for bit, and the cascade test below is the independent one.
fn gaussian_ref(c: &[f64], period: f64, poles: usize) -> Vec<f64> {
    let a = gaussian_alpha(period, libm::pow(1.414, 2.0 / poles as f64));
    let r = 1.0 - a;
    let p = |x: f64, k: f64| libm::pow(x, k);
    let mut f = [0.0_f64; 4];
    c.iter()
        .map(|&g| {
            let v = match poles {
                1 => a * g + r * f[0],
                2 => p(a, 2.0) * g + 2.0 * r * f[0] - p(r, 2.0) * f[1],
                3 => p(a, 3.0) * g + 3.0 * r * f[0] - 3.0 * p(r, 2.0) * f[1] + p(r, 3.0) * f[2],
                _ => {
                    p(a, 4.0) * g + 4.0 * r * f[0] - 6.0 * p(r, 2.0) * f[1] + 4.0 * p(r, 3.0) * f[2]
                        - p(r, 4.0) * f[3]
                }
            };
            f = [v, f[0], f[1], f[2]];
            v
        })
        .collect()
}

#[test]
fn gaussian_filter_is_the_papers_recurrence() {
    let c = closes(100.0, 2.0, 1500);
    let b = spread_bars(&c);
    for poles in 1..=4 {
        for period in [2.0, 10.0, 20.0, 144.0, 400.0] {
            let p = poles as f64;
            let want = gaussian_ref(&c, period, poles);
            let got = checked_stream(
                "gaussian_filter",
                Box::new(GaussianFilter::new(period, p).unwrap()),
                &b,
            );
            assert!(
                same(&got[0], &want),
                "gaussian_filter {poles} poles {period}: the printed recurrence"
            );
            assert_lines_same(
                "gaussian_filter: registry",
                &registry("gaussian_filter", &[period, p], &b),
                &got,
            );
            assert!(same(&series::gaussian_filter(&c, period, p).unwrap(), &got[0]), "series");
        }
    }
}

/// "It is just the multiple application of an Exponential Moving Average": `N` averages
/// `y = alpha x + (1 - alpha) y[1]` in cascade, each from 0.
#[test]
fn gaussian_filter_is_n_exponential_averages_in_cascade() {
    let c = closes(100.0, 2.0, 1200);
    for poles in 1..=4usize {
        let a = gaussian_alpha(30.0, libm::pow(1.414, 2.0 / poles as f64));
        let mut want = c.clone();
        for _ in 0..poles {
            let mut y = 0.0;
            for v in &mut want {
                y = a * *v + (1.0 - a) * y;
                *v = y;
            }
        }
        let got = series::gaussian_filter(&c, 30.0, poles as f64).unwrap();
        assert_close("gaussian_filter cascade", &got, &want, 1e-9, 1e-300);
    }
}

/// The paper's tables, `B[0]` (the input's weight) and `A[1]` (the last value's): an impulse's first
/// two outputs are `alpha^N` and `N (1 - alpha) alpha^N`. The tables were computed with `sqrt(2)`
/// (they match the formula with `2^(1/N)` to their six decimals); the filter follows the formula as
/// printed, with 1.414, which moves them by under 2e-3 of the value.
#[test]
fn gaussian_filter_meets_the_papers_tables() {
    // (poles, period, B[0], A[1])
    let table = [
        (1, 10.0, 0.455887, 0.544113),
        (2, 20.0, 0.146017, 1.235757),
        (3, 30.0, 0.037432, 1.996460),
        (4, 10.0, 0.289459, 1.066023),
        (4, 40.0, 0.008263, 2.794000),
    ];
    for (poles, period, b0, a1) in table {
        let n = f64::from(poles);
        let exact = gaussian_alpha(period, libm::pow(2.0, 1.0 / n));
        assert!(
            (libm::pow(exact, n) - b0).abs() < 1e-6,
            "{poles} poles {period}: the table is sqrt(2)'s"
        );
        let mut f = GaussianFilter::new(period, n).unwrap();
        let (first, second) = (f.next_value(1.0), f.next_value(0.0));
        assert!((first / b0 - 1.0).abs() < 2e-3, "{poles} poles {period}: B[0] {first} vs {b0}");
        assert!((second / first / a1 - 1.0).abs() < 2e-3, "{poles} poles {period}: A[1]");
    }
}

#[test]
fn gaussian_filter_has_unit_gain_and_refuses_a_fifth_pole() {
    let flat = vec![100.0; 3000];
    let got = series::gaussian_filter(&flat, 144.0, 4.0).unwrap();
    assert!((got[2999] - 100.0).abs() < 1e-6, "settles on a constant: {}", got[2999]);
    assert!(got[0] < 1.0, "starts from 0: {}", got[0]);
    assert!(GaussianFilter::new(20.0, 5.0).is_err());
    assert!(GaussianFilter::new(1.0, 4.0).is_err());
}

// ---------------------------------------------------------------- early onset trend

/// The article's recurrences: high pass, SuperSmoother, decaying peak, quotient. `zero` reads the
/// closes before the first as 0 and starts on bar 0; otherwise the first value is on bar 2.
fn eot_ref(c: &[f64], lp: f64, k: f64, zero: bool) -> Vec<f64> {
    let w = 0.707 * 2.0 * PI / 100.0;
    let a = (libm::cos(w) + libm::sin(w) - 1.0) / libm::cos(w);
    let a1 = libm::exp(-1.414 * PI / lp);
    let c2 = 2.0 * a1 * libm::cos(1.414 * PI / lp);
    let c3 = -a1 * a1;
    let c1 = 1.0 - c2 - c3;
    let start = if zero { 0 } else { 2 };
    let close = |i: usize, back: usize| if i >= back { c[i - back] } else { 0.0 };
    let (mut hp, mut filt) = (vec![0.0; c.len()], vec![0.0; c.len()]);
    let at = |v: &[f64], i: usize, back: usize| if i >= start + back { v[i - back] } else { 0.0 };
    let (mut peak, mut x) = (0.0_f64, 0.0_f64);
    let mut out = vec![f64::NAN; c.len()];
    for i in start..c.len() {
        hp[i] = (1.0 - a / 2.0) * (1.0 - a / 2.0) * (close(i, 0) - 2.0 * close(i, 1) + close(i, 2))
            + 2.0 * (1.0 - a) * at(&hp, i, 1)
            - (1.0 - a) * (1.0 - a) * at(&hp, i, 2);
        filt[i] = c1 * (hp[i] + at(&hp, i, 1)) / 2.0 + c2 * at(&filt, i, 1) + c3 * at(&filt, i, 2);
        peak *= 0.991;
        if filt[i].abs() > peak {
            peak = filt[i].abs();
        }
        if peak != 0.0 {
            x = filt[i] / peak;
        }
        out[i] = (x + k) / (k * x + 1.0);
    }
    out
}

#[test]
fn early_onset_trend_is_the_articles_quotient() {
    let c = closes(100.0, 2.0, 1500);
    let b = spread_bars(&c);
    for (lp, k) in [(30.0, 0.85), (30.0, 0.4), (6.0, 0.0), (28.0, 0.3), (15.0, -0.5)] {
        let want = eot_ref(&c, lp, k, false);
        let got =
            checked_stream("early_onset_trend", Box::new(EarlyOnsetTrend::new(lp, k).unwrap()), &b);
        assert_close("early_onset_trend", &got[0], &want, 1e-12, 1.0);
        assert!(
            got[0][..2].iter().all(|v| v.is_nan()) && !got[0][2].is_nan(),
            "first value on bar 2"
        );
        assert!(got[0][2..].iter().all(|v| v.abs() <= 1.0), "inside [-1, 1]");
        assert_lines_same(
            "early_onset_trend: registry",
            &registry("early_onset_trend", &[lp, k], &b),
            &got,
        );
        assert!(same(&series::early_onset_trend(&c, lp, k).unwrap(), &got[0]), "series");
        let off = EarlyOnsetTrend::new(lp, k).unwrap().with_zero_history(false);
        assert_lines_same(
            "early_onset_trend: option off",
            &checked_stream("eot off", Box::new(off), &b),
            &got,
        );
    }
}

/// `k = 0` is the normalised roofing filter itself: the quotient of `X` with 0 is `X`.
#[test]
fn early_onset_trend_with_k_zero_is_the_normalised_filter() {
    let c = closes(100.0, 2.0, 600);
    let got = series::early_onset_trend(&c, 20.0, 0.0).unwrap();
    assert!(got[2..].contains(&1.0) && got[2..].contains(&-1.0), "a new peak reads +-1");
}

#[test]
fn early_onset_trend_zero_history_starts_on_the_first_bar() {
    let c = closes(100.0, 2.0, 1500);
    let b = spread_bars(&c);
    for (lp, k) in [(6.0, 0.0), (28.0, 0.3), (30.0, 0.85)] {
        let indicator = EarlyOnsetTrend::new(lp, k).unwrap().with_zero_history(true);
        assert_eq!(vike_indicators::Indicator::lookback(&indicator), 0);
        let got = checked_stream("early_onset_trend zero history", Box::new(indicator), &b);
        assert_close(
            "early_onset_trend zero history",
            &got[0],
            &eot_ref(&c, lp, k, true),
            1e-12,
            1.0,
        );
        let row = registry("early_onset_trend", &[lp, k], &b);
        assert!(!same(&got[0], &row[0]), "set, it moves the row");
    }
}

// ---------------------------------------------------------------- g-h filter

/// Brookner's (1.2-11) with `T = 1`, the track started on the first number with no velocity.
fn gh_ref(y: &[f64], g: f64, h: f64) -> Vec<f64> {
    let (mut x, mut v) = (f64::NAN, 0.0);
    y.iter()
        .map(|&yn| {
            if yn.is_nan() {
                (x, v) = (f64::NAN, 0.0);
                return f64::NAN;
            }
            let pred = if x.is_nan() { yn } else { x };
            v += h * (yn - pred);
            x = pred + v + g * (yn - pred);
            x
        })
        .collect()
}

#[test]
fn gh_filter_is_brookners_prediction_equations() {
    let c = closes(100.0, 2.0, 1500);
    let b = spread_bars(&c);
    for (g, h) in
        [(0.36, 0.04), (std::f64::consts::SQRT_2, 1.0), (0.5, 0.1), (1.49, 1.0), (0.001, 0.001)]
    {
        let want = gh_ref(&c, g, h);
        let got = checked_stream("gh_filter", Box::new(GhFilter::new(g, h).unwrap()), &b);
        assert_close("gh_filter", &got[0], &want, 1e-12, 1e-300);
        assert_eq!(got[0][0], c[0], "the track starts on the first close");
        assert_lines_same("gh_filter: registry", &registry("gh_filter", &[g, h], &b), &got);
        assert!(same(&series::gh_filter(&c, g, h).unwrap(), &got[0]), "series");
    }
}

/// A g-h filter tracks a constant velocity with no error left: on a ramp its prediction becomes the
/// next value of the ramp.
#[test]
fn gh_filter_tracks_a_ramp_without_lag() {
    let ramp: Vec<f64> = (0..400).map(|i| 50.0 + 0.5 * f64::from(i)).collect();
    let got = series::gh_filter(&ramp, 0.36, 0.04).unwrap();
    assert!(
        (got[399] - (ramp[399] + 0.5)).abs() < 1e-9,
        "prediction {} for {}",
        got[399],
        ramp[399] + 0.5
    );
}

#[test]
fn gh_filter_starts_a_new_track_after_a_missing_value() {
    let y = [10.0, 11.0, 12.5, f64::NAN, 20.0, 21.0];
    let got = series::gh_filter(&y, 0.5, 0.1).unwrap();
    assert!(got[3].is_nan());
    assert_eq!(got[4], 20.0, "a new track starts on its first number");
    assert!(same(&got[4..], &series::gh_filter(&y[4..], 0.5, 0.1).unwrap()), "as from scratch");
    // Outside the stability triangle `4 - 2g - h > 0` the ranges refuse.
    assert!(GhFilter::new(1.6, 0.5).is_err());
    assert!(GhFilter::new(0.5, 1.2).is_err());
}

// ---------------------------------------------------------------- kalman filter

/// Welch and Bishop's Table 1-1 (`x- = x`, `P- = P + Q`) and Table 1-2 (`K = P- / (P- + R)`,
/// `x = x- + K (z - x-)`, `P = (1 - K) P-`) with `A = H = 1`, from `P_0 = 1` and the first number as
/// the first prior; a missing number is the time update alone.
fn kalman_ref(z: &[f64], q: f64, r: f64) -> Vec<f64> {
    let (mut x, mut p) = (f64::NAN, 1.0);
    z.iter()
        .map(|&zn| {
            if zn.is_nan() {
                if !x.is_nan() {
                    p += q;
                }
                return f64::NAN;
            }
            let x_prior = if x.is_nan() { zn } else { x };
            let p_prior = p + q;
            let k = p_prior / (p_prior + r);
            x = x_prior + k * (zn - x_prior);
            p = (1.0 - k) * p_prior;
            x
        })
        .collect()
}

#[test]
fn kalman_is_welch_and_bishops_two_tables() {
    let c = closes(100.0, 2.0, 1500);
    let b = spread_bars(&c);
    for (q, r) in [(0.001, 0.1), (0.0, 0.1), (1e-5, 0.01), (2.5, 0.0001), (1000.0, 1000.0)] {
        let got = checked_stream("kalman", Box::new(Kalman::new(q, r).unwrap()), &b);
        assert!(same(&got[0], &kalman_ref(&c, q, r)), "kalman {q} {r}: the two tables");
        assert_eq!(got[0][0], c[0], "the estimate starts on the first close");
        assert_lines_same("kalman: registry", &registry("kalman", &[q, r], &b), &got);
        assert!(same(&series::kalman(&c, q, r).unwrap(), &got[0]), "series");
    }
    assert!(Kalman::new(-0.001, 0.1).is_err(), "a negative variance");
    assert!(Kalman::new(0.001, 0.0).is_err(), "no measurement noise: 0 / 0 once P is 0");
}

/// With `Q = 0` the level is a constant and the filter is the Bayesian mean: the prior (the first
/// close, weight `1 / P_0`) and every close so far (weight `1 / R` each), `x_n = (z_0 / P_0 + sum z_i
/// / R) / (1 / P_0 + n / R)`, with error variance `1 / (1 / P_0 + n / R)`.
#[test]
fn kalman_of_a_constant_level_is_the_precision_weighted_mean() {
    let c = closes(100.0, 2.0, 800);
    let r = 0.25;
    let got = series::kalman(&c, 0.0, r).unwrap();
    // `P_0 = 1`: the prior's weight is 1.
    let mut weighted = c[0];
    for (n, &x) in got.iter().enumerate() {
        weighted += c[n] / r;
        let mean = weighted / (1.0 + (n + 1) as f64 / r);
        assert!((x - mean).abs() <= 1e-9 * mean.abs(), "bar {n}: {x} vs the weighted mean {mean}");
    }
}

/// For `Q > 0` the gain settles where the Riccati equation `P- = P- R / (P- + R) + Q` holds:
/// `P- = (Q + sqrt(Q^2 + 4 Q R)) / 2`, `K = P- / (P- + R)`; from there each step moves the estimate
/// by `K` of the residual, an exponential average with weight `K`.
#[test]
fn kalman_settles_on_the_riccati_gain() {
    let c = closes(100.0, 2.0, 3000);
    for (q, r) in [(0.001, 0.1), (0.04, 1.0), (1.0, 0.5)] {
        let got = series::kalman(&c, q, r).unwrap();
        let p = (q + (q * q + 4.0 * q * r).sqrt()) / 2.0;
        let k = p / (p + r);
        let mut checked = 0;
        for i in 2900..3000 {
            let residual = c[i] - got[i - 1];
            // A residual this large keeps the ratio's rounding near 1e-12.
            if residual.abs() > 0.05 {
                let step = (got[i] - got[i - 1]) / residual;
                assert!((step - k).abs() < 1e-9, "{q} {r} bar {i}: gain {step} vs {k}");
                checked += 1;
            }
        }
        assert!(checked > 50, "{q} {r}: only {checked} bars measured");
    }
}

/// A missing close reads `NaN`, keeps the estimate, and grows its variance by `Q`, so the next gain
/// is larger than it would have been; before the first number nothing starts.
#[test]
fn kalman_skips_a_missing_measurement_as_a_time_update() {
    let (q, r) = (0.5, 2.0);
    let z = [f64::NAN, 10.0, 11.0, f64::NAN, 13.0, 12.0];
    let got = series::kalman(&z, q, r).unwrap();
    assert!(same(&got, &kalman_ref(&z, q, r)));
    assert!(got[0].is_nan() && got[3].is_nan());
    assert_eq!(got[1], 10.0, "the first number starts the estimate");
    // By hand: P after bar 2, then two time updates before bar 4's measurement.
    let p1 = (1.0 + q) * r / (1.0 + q + r);
    let p2_prior = p1 + q;
    let k2 = p2_prior / (p2_prior + r);
    let p2 = (1.0 - k2) * p2_prior;
    let k4 = (p2 + 2.0 * q) / (p2 + 2.0 * q + r);
    assert!((got[4] - (got[2] + k4 * (13.0 - got[2]))).abs() < 1e-12, "bar 4 after the gap");
    let unbroken = series::kalman(&[10.0, 11.0, 13.0], q, r).unwrap();
    assert!(got[4] > unbroken[2], "the gap's larger gain moves it further toward 13");
}
