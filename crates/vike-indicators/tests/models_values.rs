//! Value tests for the indicators read off a model (`src/indicators/models.rs`): `garch_volatility`,
//! `hmm_regime`, `dominant_cycle`, `haar_detail_energy` and `adf`, each against its published definition
//! written again here. Where the reference takes the module's own arithmetic in the same order the
//! check is bit for bit; where it takes the textbook form instead (the HMM's densities unscaled, the
//! RiskMetrics average, Parseval's sum) the tolerance is the rounding of that other path, and the
//! comment says why.
//!
//! `on_bar == vectorize`, the warm-up claims and the parameter grid are held for every registry row
//! by `tests/parity`; this file is about the NUMBERS, plus each `series::*` call giving the stream's
//! bits.

use vike_indicators::indicators::{
    Adf, DominantCycle, GarchVolatility, HaarDetailEnergy, HmmRegime,
};
use vike_indicators::{lookback, make_with, series};
use vike_marketdata::Bar;
use vike_math::stationarity_tests::{Deterministic, augmented_dickey_fuller};

/// Bars that only carry a close.
fn bars(closes: &[f64]) -> Vec<Bar> {
    closes
        .iter()
        .enumerate()
        .map(|(i, &c)| Bar::ohlcv(i as i64 * 60_000, c, c, c, c, 1.0))
        .collect()
}

fn same(a: f64, b: f64) -> bool {
    a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan())
}

fn log_returns(closes: &[f64]) -> Vec<f64> {
    closes.windows(2).map(|w| libm::log(w[1] / w[0])).collect()
}

/// Closes from log returns, starting at 100.
fn closes_from(returns: &[f64]) -> Vec<f64> {
    let mut closes = vec![100.0];
    for r in returns {
        let last = *closes.last().expect("starts with one close");
        closes.push(last * libm::exp(*r));
    }
    closes
}

// ---------------------------------------------------------------- garch

/// Bollerslev's recurrence, started at the unconditional variance `omega / (1 - alpha - beta)`.
#[test]
fn garch_volatility_is_bollerslevs_recurrence_from_the_unconditional_variance() {
    let closes = [100.0, 101.0, 99.5, 102.0, 102.0, 98.0];
    let (omega, alpha, beta) = (0.000001, 0.1, 0.85);
    let mut variance = omega / (1.0 - (alpha + beta));
    let mut want = vec![f64::NAN];
    for r in log_returns(&closes) {
        variance = omega + alpha * (r * r) + beta * variance;
        want.push(variance.sqrt());
    }
    let mut g = GarchVolatility::new(omega, alpha, beta).expect("inside the row's ranges");
    let got: Vec<f64> = closes.iter().map(|&c| g.next_value(c)).collect();
    for (i, (g, w)) in got.iter().zip(&want).enumerate() {
        assert!(same(*g, *w), "bar {i}: got {g:e}, want {w:e}");
    }
    let whole = series::garch_volatility(&closes, omega, alpha, beta).expect("inside the ranges");
    assert!(whole.iter().zip(&got).all(|(a, b)| same(*a, *b)), "series::garch_volatility");
    assert_eq!(lookback("garch_volatility", &[omega, alpha, beta]), Some(1));
}

/// With `omega = 0`, `alpha = 1 - lambda` and `beta = lambda` (an integrated model, so the start is
/// `r[1]^2`) it is RiskMetrics' average, the `rvol_ewma` row. Not bit for bit: GARCH's first
/// variance is `alpha r^2 + beta r^2` where the average's is `r^2`, one rounding apart, and that
/// rounding decays by `lambda` a bar.
#[test]
fn garch_volatility_with_no_constant_and_unit_persistence_is_the_riskmetrics_average() {
    let lambda = 0.94;
    let closes: Vec<f64> = (0..300).map(|i| 100.0 + 5.0 * (f64::from(i) * 0.37).sin()).collect();
    let b = bars(&closes);
    let garch = make_with("garch_volatility", &[0.0, 1.0 - lambda, lambda]).unwrap().vectorize(&b);
    let ewma = make_with("rvol_ewma", &[lambda]).unwrap().vectorize(&b);
    for (i, (g, e)) in garch[0].iter().zip(&ewma[0]).enumerate() {
        if i == 0 {
            assert!(g.is_nan() && e.is_nan(), "bar 0 has no return");
            continue;
        }
        assert!((g - e).abs() <= 1e-14 * e.abs(), "bar {i}: garch {g:e}, rvol_ewma {e:e}");
    }
}

/// A bar with no return reads `NaN` and leaves the forecast: the return after the gap folds into
/// the forecast the last return left.
#[test]
fn garch_volatility_skips_a_bar_with_no_return() {
    let (omega, alpha, beta) = (0.000001, 0.1, 0.85);
    let mut g = GarchVolatility::new(omega, alpha, beta).unwrap();
    let out: Vec<f64> =
        [100.0, 101.0, f64::NAN, 103.0, 104.0].iter().map(|&c| g.next_value(c)).collect();
    assert!(out[0].is_nan() && out[2].is_nan() && out[3].is_nan(), "{out:?}");
    let r1 = libm::log(101.0_f64 / 100.0);
    let v1 = omega + alpha * (r1 * r1) + beta * (omega / (1.0 - (alpha + beta)));
    let r4 = libm::log(104.0_f64 / 103.0);
    let v4 = omega + alpha * (r4 * r4) + beta * v1;
    assert!(same(out[1], v1.sqrt()) && same(out[4], v4.sqrt()), "{out:?}");
}

// ---------------------------------------------------------------- hmm

/// When the two states emit the same density, a return says nothing: the belief is the chain alone,
/// which started at its stationary law `(1 - stay_0) / (2 - stay_0 - stay_1)` and stays there.
#[test]
fn hmm_regime_is_the_chain_alone_when_the_states_look_alike() {
    let closes: Vec<f64> = (0..200).map(|i| 100.0 + 3.0 * (f64::from(i) * 0.41).sin()).collect();
    let got = series::hmm_regime(&closes, 0.0, 0.02, 0.9, 0.0, 0.02, 0.7).unwrap();
    assert!(got[0].is_nan());
    let stationary = 0.1 / (0.1 + 0.3);
    for (i, p) in got.iter().enumerate().skip(1) {
        assert!((p - stationary).abs() < 1e-15, "bar {i}: {p} is not the stationary 0.25");
    }
}

/// One step by hand, with the textbook densities `exp(-(r - mean)^2 / (2 sigma^2)) / sigma`
/// (the module scales both by the larger before `exp`, so the two agree to rounding).
#[test]
fn hmm_regime_is_hamiltons_filter_step_by_step() {
    let closes = [100.0, 101.0, 100.2, 103.5, 99.0, 99.1];
    let (mean, sigma, stay) = ([0.0005, -0.001], [0.01, 0.03], [0.98, 0.95]);
    let density = |r: f64, i: usize| {
        libm::exp(-(r - mean[i]) * (r - mean[i]) / (2.0 * sigma[i] * sigma[i])) / sigma[i]
    };
    let mut p = (1.0 - stay[0]) / ((1.0 - stay[0]) + (1.0 - stay[1]));
    let mut want = vec![f64::NAN];
    for r in log_returns(&closes) {
        let q1 = (1.0 - p) * (1.0 - stay[0]) + p * stay[1];
        let (w0, w1) = ((1.0 - q1) * density(r, 0), q1 * density(r, 1));
        p = w1 / (w0 + w1);
        want.push(p);
    }
    let mut h = HmmRegime::new(mean[0], sigma[0], stay[0], mean[1], sigma[1], stay[1]).unwrap();
    for (i, (&c, w)) in closes.iter().zip(&want).enumerate() {
        let got = h.next_value(c);
        if i == 0 {
            assert!(got.is_nan());
        } else {
            assert!((got - w).abs() < 1e-12, "bar {i}: got {got}, want {w}");
        }
    }
}

/// Calm returns pull the belief to state 0 (sigma 0.01), turbulent ones to state 1 (sigma 0.03).
/// Not to 0: a calm return is at best 3 times likelier under state 0 (the ratio of the sigmas), and
/// the chain leaks 2% a bar into state 1, so the belief settles near 1%.
#[test]
fn hmm_regime_follows_a_change_of_regime() {
    let returns: Vec<f64> = (0..200)
        .map(|i| {
            let size = if i < 100 { 0.004 } else { 0.05 };
            if i % 2 == 0 { size } else { -size }
        })
        .collect();
    let got = make_with("hmm_regime", &[]).unwrap().vectorize(&bars(&closes_from(&returns)));
    let (calm, turbulent) = (got[0][100], got[0][200]);
    assert!(calm < 0.05, "after 100 calm returns P(state 1) = {calm}");
    assert!(turbulent > 0.99, "after 100 turbulent returns P(state 1) = {turbulent}");
}

/// Stays of 1 make a state certain once the other is ruled out: a later return far likelier under
/// the ruled-out state, where the likely state's weight underflows, leaves the belief where it was
/// (the prediction), never `NaN`.
#[test]
fn hmm_regime_keeps_a_certainty() {
    let mut h = HmmRegime::new(0.0, 0.000001, 1.0, 0.0, 1.0, 1.0).unwrap();
    assert!(h.next_value(100.0).is_nan(), "the first bar has no return");
    // Returns of 0 are a million times likelier under state 0 (sigma 1e-6) each: state 1's
    // probability falls by that factor a bar until it is exactly 0.
    let mut settled = f64::NAN;
    for _ in 0..100 {
        settled = h.next_value(100.0);
    }
    assert!(settled == 0.0, "{settled}");
    // A 10% move is impossible under state 0 and state 1 is ruled out for good: nothing moves.
    let after = h.next_value(110.0);
    assert!(after == 0.0, "{after}");
}

// ---------------------------------------------------------------- dominant cycle

/// A 20-bar cycle riding a trend: the detrended spectrum peaks at 20. The window holds 6.4 cycles,
/// so the peak is not on a whole number of cycles and the parabola places it within half a bar.
#[test]
fn dominant_cycle_finds_a_twenty_bar_cycle_on_a_trend() {
    let closes: Vec<f64> = (0..400)
        .map(|i| {
            let t = f64::from(i);
            100.0 + 0.05 * t + 2.0 * (2.0 * std::f64::consts::PI * t / 20.0).sin()
        })
        .collect();
    let got = series::dominant_cycle(&closes, 128.0, 8.0, 48.0).unwrap();
    assert_eq!(got.iter().take_while(|v| v.is_nan()).count(), 127);
    for (i, v) in got.iter().enumerate().skip(127) {
        assert!((v - 20.0).abs() < 0.5, "bar {i}: dominant cycle {v}, want about 20");
    }
    let mut d = DominantCycle::new(128.0, 8.0, 48.0).unwrap();
    assert!(closes.iter().zip(&got).all(|(&c, &w)| same(d.next_value(c), w)), "stream != series");
}

/// A window that does not move has no cycle.
#[test]
fn dominant_cycle_of_a_flat_window_is_nan() {
    let got = series::dominant_cycle(&[100.0; 80], 16.0, 4.0, 12.0).unwrap();
    assert!(got.iter().all(|v| v.is_nan()), "{got:?}");
}

/// The answer stays inside the searched range, and a range set the wrong way round searches
/// `min_period` alone.
#[test]
fn dominant_cycle_stays_in_its_range() {
    let closes: Vec<f64> =
        (0..200).map(|i| 100.0 + (f64::from(i) * 1.3).sin() * 4.0 + f64::from(i % 7)).collect();
    let got = series::dominant_cycle(&closes, 32.0, 6.0, 20.0).unwrap();
    assert!(got.iter().skip(31).all(|v| (6.0..=20.0).contains(v)), "{got:?}");
    let reversed = series::dominant_cycle(&closes, 32.0, 12.0, 5.0).unwrap();
    assert!(reversed.iter().skip(31).all(|v| *v == 12.0), "{reversed:?}");
}

// ---------------------------------------------------------------- haar

/// Level 1 over four returns, by hand: `((r1 - r2)^2 + (r3 - r4)^2) / 2`.
#[test]
fn haar_detail_energy_by_hand() {
    let closes = [100.0, 101.0, 99.0, 102.0, 98.0, 98.5];
    let r = log_returns(&closes);
    let d = |a: f64, b: f64| (a - b) * (a - b);
    let want_4 = (d(r[0], r[1]) + d(r[2], r[3])) / 2.0;
    let want_5 = (d(r[1], r[2]) + d(r[3], r[4])) / 2.0;
    let got = series::haar_detail_energy(&closes, 4.0, 1.0).unwrap();
    assert!(got[..4].iter().all(|v| v.is_nan()), "{got:?}");
    assert!(same(got[4], want_4) && same(got[5], want_5), "{got:?} want {want_4:e}, {want_5:e}");
    let mut h = HaarDetailEnergy::new(4.0, 1.0).unwrap();
    assert!(closes.iter().zip(&got).all(|(&c, &w)| same(h.next_value(c), w)), "stream != series");
}

/// Parseval: over a window of 8 returns, the detail energies of levels 1, 2 and 3 plus the energy of
/// the mean, `(sum r)^2 / 8`, are the window's `sum r^2`. Four folds against one, so to the rounding
/// of a few dozen operations on numbers near `1e-4`.
#[test]
fn haar_detail_energy_splits_the_window_energy_by_scale() {
    let closes = [100.0, 100.7, 99.9, 101.2, 101.0, 102.4, 101.1, 100.6, 101.9];
    let r = log_returns(&closes);
    let last = closes.len() - 1;
    let energy: f64 = (1..=3)
        .map(|level| series::haar_detail_energy(&closes, 8.0, f64::from(level)).unwrap()[last])
        .sum();
    let mean_energy = r.iter().sum::<f64>() * r.iter().sum::<f64>() / 8.0;
    let total: f64 = r.iter().map(|x| x * x).sum();
    assert!(
        (energy + mean_energy - total).abs() <= 1e-13 * total,
        "{energy} + {mean_energy} != {total}"
    );
}

/// The window is the most whole blocks that fit in `length`, at least one.
#[test]
fn haar_detail_energy_window_is_whole_blocks() {
    assert_eq!(lookback("haar_detail_energy", &[10.0, 2.0]), Some(8));
    assert_eq!(lookback("haar_detail_energy", &[3.0, 3.0]), Some(8));
    assert_eq!(lookback("haar_detail_energy", &[64.0, 1.0]), Some(64));
    let flat = series::haar_detail_energy(&[100.0; 20], 8.0, 2.0).unwrap();
    assert!(flat[8..].iter().all(|v| *v == 0.0), "{flat:?}");
}

// ---------------------------------------------------------------- adf

/// The row is `vike_math::stationarity_tests`'s `augmented_dickey_fuller` over the window of log closes, bit for bit, `NaN`
/// through the warm-up; closes scattered around one level (white noise in the log) read far below the
/// 5% critical value, and a flat window is `NaN`.
#[test]
fn adf_is_the_one_statistic_over_the_window_of_log_closes() {
    let mut state = 7_u64;
    let closes: Vec<f64> = (0..150)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let u = (state >> 11) as f64 / (1u64 << 53) as f64 - 0.5;
            100.0 * libm::exp(0.01 * u)
        })
        .collect();
    let got = series::adf(&closes, 100.0, 1.0).unwrap();
    assert!(got[..99].iter().all(|v| v.is_nan()), "warm-up: {:?}", &got[..99]);
    let logs: Vec<f64> = closes.iter().map(|&c| libm::log(c)).collect();
    for k in 99..closes.len() {
        let want = augmented_dickey_fuller(&logs[k - 99..=k], 1, Deterministic::Constant);
        assert!(same(got[k], want), "bar {k}: got {:e}, want {want:e}", got[k]);
        assert!(
            got[k] < -2.86,
            "bar {k}: white noise around a level rejects the unit root: {}",
            got[k]
        );
    }
    let mut row = Adf::new(100.0, 1.0).unwrap();
    assert!(closes.iter().zip(&got).all(|(&c, &w)| same(row.next_value(c), w)), "stream != series");
    assert!(series::adf(&[100.0; 120], 100.0, 1.0).unwrap().iter().all(|v| v.is_nan()));
    assert_eq!(lookback("adf", &[20.0, 8.0]), Some(19));
}
