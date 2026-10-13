//! `tests/pairs_parity.rs` proves stream == batch; it does NOT prove the
//! arithmetic is right. These pin the MATH against closed-form answers.
use super::*;

fn bar(ts: usize, close: f64) -> Bar {
    Bar {
        ts: ts as i64 * 3_600_000,
        open: close,
        high: close,
        low: close,
        close,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

fn series(v: &[f64]) -> Vec<Bar> {
    v.iter().enumerate().map(|(i, &c)| bar(i, c)).collect()
}

/// The backward-compatibility contract: `beta = 1.0` must reproduce the
/// original hardcoded `close − benchmark` spread BIT-FOR-BIT (multiplying by
/// 1.0 is exact in IEEE-754). Asserted against a verbatim copy of the
/// pre-change kernel, not merely eyeballed.
#[test]
fn spread_zscore_beta_one_is_bit_identical_to_unhedged() {
    fn legacy(a: &[Bar], b: &[Bar], period: usize) -> Vec<f64> {
        let (ca, cb) = (closes(a), closes(b));
        let n = ca.len();
        let s: Vec<f64> = (0..n).map(|i| ca[i] - cb[i]).collect();
        let mut out = vec![f64::NAN; n];
        let (mut run_sum, mut run_sum2) = (0.0, 0.0);
        for i in 0..n {
            run_sum += s[i];
            run_sum2 += s[i] * s[i];
            if i >= period {
                run_sum -= s[i - period];
                run_sum2 -= s[i - period] * s[i - period];
            }
            if i + 1 >= period {
                let mean = run_sum / period as f64;
                let var = run_sum2 / period as f64 - mean * mean;
                let sd = var.max(0.0).sqrt();
                if sd != 0.0 {
                    out[i] = (s[i] - mean) / sd;
                }
            }
        }
        out
    }

    let a: Vec<Bar> =
        series(&(0..120).map(|i| 100.0 + (i as f64 * 0.11).sin() * 7.0).collect::<Vec<_>>());
    let b: Vec<Bar> =
        series(&(0..120).map(|i| 50.0 + (i as f64 * 0.07).cos() * 3.0).collect::<Vec<_>>());

    let got = &batch_spread_zscore(&a, &b, 20, 1.0)[0];
    let want = legacy(&a, &b, 20);
    assert_eq!(got.len(), want.len());
    for (i, (&g, &w)) in got.iter().zip(want.iter()).enumerate() {
        assert!(
            (g.is_nan() && w.is_nan()) || g.to_bits() == w.to_bits(),
            "idx {i}: hedged(beta=1) {g:?} != legacy {w:?}"
        );
    }
}

/// A non-unit beta must actually change the spread — proves the parameter is
/// wired through and not silently ignored.
#[test]
fn spread_zscore_beta_changes_the_spread() {
    let a: Vec<Bar> =
        series(&(0..80).map(|i| 100.0 + (i as f64 * 0.2).sin() * 5.0).collect::<Vec<_>>());
    let b: Vec<Bar> =
        series(&(0..80).map(|i| 20.0 + (i as f64 * 0.2).sin() * 5.0).collect::<Vec<_>>());
    let one = batch_spread_zscore(&a, &b, 20, 1.0)[0].clone();
    let two = batch_spread_zscore(&a, &b, 20, 2.0)[0].clone();
    assert!(
        one.iter()
            .zip(two.iter())
            .any(|(x, y)| { !(x.is_nan() && y.is_nan()) && x.to_bits() != y.to_bits() }),
        "beta had no effect on the z-score"
    );
}

/// Closed form: on a noiseless geometric decay `s_t = 0.5·s_{t−1}` the
/// regression slope is exactly `λ = −0.5`, so the half-life is `−ln2/λ =
/// 2·ln2 ≈ 1.3863` bars. Benchmark held constant so `beta = 1` leaves the
/// spread equal to the decaying series.
#[test]
fn half_life_recovers_known_decay_rate() {
    let mut s = 64.0f64;
    let mut a_close = Vec::new();
    for _ in 0..40 {
        a_close.push(100.0 + s);
        s *= 0.5;
    }
    let a = series(&a_close);
    let b = series(&vec![100.0; a_close.len()]);

    let hl = &batch_half_life(&a, &b, 10, 1.0)[0];
    let last = hl.iter().rev().find(|v| !v.is_nan()).copied().expect("a half-life");
    let expected = 2.0 * std::f64::consts::LN_2;
    assert!((last - expected).abs() < 1e-9, "half-life {last} != {expected}");
}

/// A trending spread has `λ >= 0` — no finite half-life. NaN is the correct
/// answer and the stand-down signal, not a warm-up artifact.
#[test]
fn half_life_is_nan_when_not_mean_reverting() {
    let a = series(&(0..60).map(|i| 100.0 + i as f64).collect::<Vec<_>>());
    let b = series(&vec![100.0; 60]);
    let hl = &batch_half_life(&a, &b, 10, 1.0)[0];
    assert!(hl.iter().all(|v| v.is_nan()), "a pure linear trend must yield no half-life");
}

/// A degenerate (constant) spread has zero variance and therefore no slope.
#[test]
fn half_life_is_nan_on_constant_spread() {
    let a = series(&vec![100.0; 40]);
    let b = series(&vec![100.0; 40]);
    assert!(batch_half_life(&a, &b, 10, 1.0)[0].iter().all(|v| v.is_nan()));
}

/// With `close = 2·benchmark` exactly, the filter must converge on the true
/// hedge ratio 2.0.
#[test]
fn kalman_beta_converges_to_true_hedge_ratio() {
    let b_close: Vec<f64> = (0..300).map(|i| 100.0 + (i as f64 * 0.05).sin() * 10.0).collect();
    let a_close: Vec<f64> = b_close.iter().map(|x| 2.0 * x).collect();
    let a = series(&a_close);
    let b = series(&b_close);

    let out = &batch_kalman_beta(&a, &b, 0.0001, 0.001)[0];
    let last = out.last().copied().unwrap();
    assert!((last - 2.0).abs() < 0.05, "kalman beta {last} did not converge to 2.0");
    assert!(out.iter().all(|v| v.is_finite()), "kalman beta produced a non-finite value");
}

/// Tracking check: when the true ratio SHIFTS mid-series, the filter must
/// move toward the new value — the whole point of a time-varying beta.
#[test]
fn kalman_beta_tracks_a_regime_shift() {
    let b_close: Vec<f64> = (0..600).map(|i| 100.0 + (i as f64 * 0.05).sin() * 10.0).collect();
    let a_close: Vec<f64> =
        b_close.iter().enumerate().map(|(i, x)| if i < 300 { 2.0 * x } else { 3.0 * x }).collect();
    let a = series(&a_close);
    let b = series(&b_close);

    let out = &batch_kalman_beta(&a, &b, 0.01, 0.001)[0];
    let before = out[299];
    let after = out[599];
    assert!((before - 2.0).abs() < 0.1, "pre-shift beta {before} != ~2.0");
    assert!(after > before, "beta did not move toward the new ratio");
}
