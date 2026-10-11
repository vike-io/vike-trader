//! The oracle for `crates/vike-indicators/src/realized_vol.rs` — net-new code with no Python twin,
//! so nothing else pins its values.
//!
//! Two kinds of test, and the difference is the point:
//!
//! - **Hand-unrolled folds**, compared by EXACT bits. Each rebuilds an estimator from the same
//!   primitives (`libm::log`, `*`, `+`, `/`, `f64::sqrt`) in the same order the module folds them,
//!   so a changed formula, weight, denominator or fold order fails here. ⚠ The unrolls spell
//!   `libm::log`, never `f64::ln`: an `f64::ln` unroll would assert that the running box's CRT
//!   agrees with the portable implementation, a test of the platform rather than of the recursion.
//! - **Properties** — zero on a flat series, scale invariance, the `sqrt` scaling of the year basis,
//!   and the textbook drift relations between the estimators — compared bit-for-bit where the
//!   arithmetic is exact, and by a stated relative bound where it is a real-arithmetic identity
//!   that two different f64 folds only approximate.
//!
//! `on_bar == vectorize` for the six `rvol_*` registry rows is held by `tests/parity.rs` like every
//! other row, but for this family it is true by CONSTRUCTION (`Estimator::series` folds
//! `RealizedVol::update`), so those rows are not evidence about the values; this file is.

use std::f64::consts::LN_2;

use vike_indicators::realized_vol::{
    DAYS_PER_YEAR, Estimator, RISKMETRICS_LAMBDA, RealizedVol, annualize, deannualize,
};
use vike_indicators::{make, make_with};
use vike_marketdata::Bar;

fn bar(open: f64, high: f64, low: f64, close: f64) -> Bar {
    Bar {
        ts: 0,
        open,
        high,
        low,
        close,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// A bar that only has a close (the close-to-close estimators read nothing else).
fn close(c: f64) -> Bar {
    bar(c, c, c, c)
}

fn assert_bits(what: &str, got: f64, expect: f64) {
    assert_eq!(got.to_bits(), expect.to_bits(), "{what}: got {got:e}, expected {expect:e}");
}

fn assert_nan(what: &str, got: f64) {
    assert!(got.is_nan(), "{what}: expected NaN, got {got:e}");
}

/// Every estimator at a small window, for the properties that hold across the family.
fn all(window: usize) -> [Estimator; 6] {
    [
        Estimator::Ewma { lambda: RISKMETRICS_LAMBDA },
        Estimator::CloseToClose { window },
        Estimator::Parkinson { window },
        Estimator::GarmanKlass { window },
        Estimator::RogersSatchell { window },
        Estimator::YangZhang { window },
    ]
}

/// Deterministic OHLC bars from an LCG, `+ - * /` only, with a real opening gap so Yang–Zhang's
/// overnight term is not identically zero.
fn lcg_bars(n: usize, seed: u64) -> Vec<Bar> {
    let mut s = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15).wrapping_add(0x2545_f491_4f6c_dd1d);
    let mut next = move || {
        s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (s >> 11) as f64 / (1u64 << 53) as f64
    };
    let mut out = Vec::with_capacity(n);
    let mut prev = 100.0f64;
    for _ in 0..n {
        let open = prev * (1.0 + (next() - 0.5) * 0.01);
        let close = open * (1.0 + (next() - 0.5) * 0.03);
        let high = open.max(close) * (1.0 + next() * 0.01);
        let low = open.min(close) * (1.0 - next() * 0.01);
        out.push(bar(open, high, low, close));
        prev = close;
    }
    out
}

// ================================ EWMA ======================================

#[test]
fn ewma_matches_hand_unrolled_recursion() {
    // Closes 100 -> 101 -> 99 -> 102 at λ = 0.94.
    let mut e = RealizedVol::new(Estimator::Ewma { lambda: RISKMETRICS_LAMBDA });
    assert_nan("no return on the first bar", e.update(&close(100.0)));
    assert!(e.value().is_none());

    let r1 = libm::log(101.0f64 / 100.0);
    let mut v = r1 * r1; // the first return SEEDS the variance
    assert_bits("seed", e.update(&close(101.0)), v.sqrt());

    // `(1.0 - 0.94)` is spelled as the recursion states it: in f64 it is NOT the literal `0.06`.
    let r2 = libm::log(99.0f64 / 101.0);
    v = 0.94 * v + (1.0 - 0.94) * (r2 * r2);
    assert_bits("second fold", e.update(&close(99.0)), v.sqrt());

    let r3 = libm::log(102.0f64 / 99.0);
    v = 0.94 * v + (1.0 - 0.94) * (r3 * r3);
    assert_bits("third fold", e.update(&close(102.0)), v.sqrt());
    assert_bits("value()", e.value().unwrap(), v.sqrt());
    assert_bits(
        "annualized at the caller's basis",
        e.annualized(DAYS_PER_YEAR).unwrap(),
        v.sqrt() * 365.0f64.sqrt(),
    );
}

#[test]
fn a_bad_close_breaks_the_return_chain_like_hvol() {
    // The vike-options original took the next return against the last GOOD price; this family
    // follows `batch_hvol` instead: a return exists only between two consecutive usable closes.
    let mut e = RealizedVol::new(Estimator::Ewma { lambda: RISKMETRICS_LAMBDA });
    let outs: Vec<f64> = [100.0, f64::NAN, 0.0, -5.0, f64::INFINITY, 101.0, 103.0]
        .iter()
        .map(|&c| e.update(&close(c)))
        .collect();
    for (i, v) in outs.iter().take(6).enumerate() {
        assert_nan(&format!("bar {i}: no return across or out of a bad close"), *v);
    }
    let r = libm::log(103.0f64 / 101.0); // the FIRST return is 101 -> 103, never 100 -> 101
    assert_bits("first return after the gap seeds", outs[6], (r * r).sqrt());
}

#[test]
#[should_panic(expected = "lambda must be in (0, 1)")]
fn ewma_rejects_a_lambda_of_one() {
    let _ = RealizedVol::new(Estimator::Ewma { lambda: 1.0 });
}

// ============================ close-to-close =================================

#[test]
fn close_to_close_matches_hand_computed_window() {
    let est = Estimator::CloseToClose { window: 3 };
    let out = est.series(&[100.0, 101.0, 99.0, 102.0, 103.0].map(close));
    for (i, v) in out.iter().take(3).enumerate() {
        assert_nan(&format!("bar {i}: fewer than 3 returns"), *v);
    }
    let r1 = libm::log(101.0f64 / 100.0);
    let r2 = libm::log(99.0f64 / 101.0);
    let r3 = libm::log(102.0f64 / 99.0);
    let r4 = libm::log(103.0f64 / 102.0);
    let m = (r1 + r2 + r3) / 3.0;
    let ss = (r1 - m) * (r1 - m) + (r2 - m) * (r2 - m) + (r3 - m) * (r3 - m);
    assert_bits("full window, SAMPLE variance (n - 1)", out[3], (ss / 2.0).sqrt());
    let m = (r2 + r3 + r4) / 3.0;
    let ss = (r2 - m) * (r2 - m) + (r3 - m) * (r3 - m) + (r4 - m) * (r4 - m);
    assert_bits("rolled window evicts the oldest return", out[4], (ss / 2.0).sqrt());
}

#[test]
fn close_to_close_windows_the_defined_returns_across_a_gap() {
    // Bar 3's close is unusable, so bars 3 AND 4 have no return; the window then spans the gap.
    let out = Estimator::CloseToClose { window: 3 }
        .series(&[100.0, 101.0, 99.0, 0.0, 102.0, 103.0, 104.0].map(close));
    for (i, v) in out.iter().take(5).enumerate() {
        assert_nan(&format!("bar {i}"), *v);
    }
    let r1 = libm::log(101.0f64 / 100.0);
    let r2 = libm::log(99.0f64 / 101.0);
    let r5 = libm::log(103.0f64 / 102.0);
    let r6 = libm::log(104.0f64 / 103.0);
    let m = (r1 + r2 + r5) / 3.0;
    let ss = (r1 - m) * (r1 - m) + (r2 - m) * (r2 - m) + (r5 - m) * (r5 - m);
    assert_bits("window [r1, r2, r5]", out[5], (ss / 2.0).sqrt());
    let m = (r2 + r5 + r6) / 3.0;
    let ss = (r2 - m) * (r2 - m) + (r5 - m) * (r5 - m) + (r6 - m) * (r6 - m);
    assert_bits("window [r2, r5, r6]", out[6], (ss / 2.0).sqrt());
}

#[test]
#[should_panic(expected = "window must be >= 2")]
fn a_window_of_one_is_refused() {
    let _ = RealizedVol::new(Estimator::CloseToClose { window: 1 });
}

/// `hvol` is the POPULATION stdev of the same returns, annualized and in percent. The two are
/// different f64 folds, so the relation is asserted to a stated bound, never by bits.
#[test]
fn hvol_is_close_to_close_rescaled_not_the_same_number() {
    let bars = lcg_bars(300, 7);
    let window = 20usize;
    let c2c = Estimator::CloseToClose { window }.series(&bars);
    let hvol = make_with("hvol", &[window as f64, 365.0]).unwrap().vectorize(&bars).remove(0);
    let scale = ((window as f64 - 1.0) / window as f64).sqrt() * 365.0f64.sqrt() * 100.0;
    let mut checked = 0;
    for (i, (&a, &h)) in c2c.iter().zip(hvol.iter()).enumerate() {
        assert_eq!(a.is_nan(), h.is_nan(), "bar {i}: both warm up on the same bar");
        if a.is_nan() {
            continue;
        }
        assert_ne!(a.to_bits(), h.to_bits(), "bar {i}: they are different quantities");
        let rel = ((a * scale - h) / h).abs();
        assert!(
            rel < 1e-12,
            "bar {i}: hvol {h} vs c2c·√((n-1)/n)·√365·100 {} (rel {rel:e})",
            a * scale
        );
        checked += 1;
    }
    assert!(checked > 200, "the relation must be checked on most bars, saw {checked}");
}

// ============================ range estimators ===============================

#[test]
fn parkinson_matches_hand_unrolled_window() {
    let (b0, b1) = (bar(100.0, 102.0, 99.0, 101.0), bar(101.0, 103.5, 100.25, 100.5));
    let out = Estimator::Parkinson { window: 2 }.series(&[b0, b1]);
    assert_nan("bar 0: one term < window", out[0]);
    let u0 = libm::log(102.0f64 / 99.0);
    let u1 = libm::log(103.5f64 / 100.25);
    let mean = (u0 * u0 + u1 * u1) / 2.0;
    assert_bits("parkinson", out[1], ((1.0 / (4.0 * LN_2)) * mean).sqrt());
}

#[test]
fn parkinson_reads_only_high_and_low() {
    // An unusable open and close do not remove a Parkinson term.
    let out = Estimator::Parkinson { window: 2 }
        .series(&[bar(f64::NAN, 102.0, 99.0, 0.0), bar(-1.0, 103.5, 100.25, f64::NAN)]);
    assert!(!out[1].is_nan(), "parkinson needs H and L only, got {}", out[1]);
}

#[test]
fn garman_klass_matches_hand_unrolled_window() {
    let (b0, b1) = (bar(100.0, 102.0, 99.0, 101.0), bar(101.0, 103.5, 100.25, 100.5));
    let out = Estimator::GarmanKlass { window: 2 }.series(&[b0, b1]);
    assert_nan("bar 0", out[0]);
    let k = 2.0 * LN_2 - 1.0;
    let (hl0, co0) = (libm::log(102.0f64 / 99.0), libm::log(101.0f64 / 100.0));
    let (hl1, co1) = (libm::log(103.5f64 / 100.25), libm::log(100.5f64 / 101.0));
    let g0 = 0.5 * (hl0 * hl0) - k * (co0 * co0);
    let g1 = 0.5 * (hl1 * hl1) - k * (co1 * co1);
    assert_bits("garman-klass", out[1], ((g0 + g1) / 2.0).sqrt());
}

#[test]
fn rogers_satchell_matches_hand_unrolled_window() {
    let (b0, b1) = (bar(100.0, 102.0, 99.0, 101.0), bar(101.0, 103.5, 100.25, 100.5));
    let out = Estimator::RogersSatchell { window: 2 }.series(&[b0, b1]);
    assert_nan("bar 0", out[0]);
    let rs = |o: f64, h: f64, l: f64, c: f64| {
        libm::log(h / c) * libm::log(h / o) + libm::log(l / c) * libm::log(l / o)
    };
    let (s0, s1) = (rs(100.0, 102.0, 99.0, 101.0), rs(101.0, 103.5, 100.25, 100.5));
    assert_bits("rogers-satchell", out[1], ((s0 + s1) / 2.0).sqrt());
}

#[test]
fn yang_zhang_matches_hand_unrolled_window() {
    let bars = [
        bar(100.0, 102.0, 99.0, 101.0),
        bar(101.5, 103.5, 100.25, 100.5),
        bar(100.0, 101.0, 98.5, 99.75),
    ];
    let out = Estimator::YangZhang { window: 2 }.series(&bars);
    assert_nan("bar 0: no previous close", out[0]);
    assert_nan("bar 1: one term < window", out[1]);

    let rs = |b: &Bar| {
        libm::log(b.high / b.close) * libm::log(b.high / b.open)
            + libm::log(b.low / b.close) * libm::log(b.low / b.open)
    };
    let (o1, o2) = (libm::log(101.5f64 / 101.0), libm::log(100.0f64 / 100.5));
    let (c1, c2) = (libm::log(100.5f64 / 101.5), libm::log(99.75f64 / 100.0));
    let sample_var = |a: f64, b: f64| {
        let m = (a + b) / 2.0;
        // `/ (n - 1)` is `/ 1.0` at n = 2, which is exact, so it is not written.
        (a - m) * (a - m) + (b - m) * (b - m)
    };
    let k = 0.34 / (1.34 + (2.0 + 1.0) / (2.0 - 1.0));
    let var = sample_var(o1, o2)
        + k * sample_var(c1, c2)
        + (1.0 - k) * ((rs(&bars[1]) + rs(&bars[2])) / 2.0);
    assert_bits("yang-zhang", out[2], var.sqrt());
}

// ================================ properties =================================

#[test]
fn a_flat_series_is_exactly_zero_for_every_estimator() {
    let bars = vec![close(0.1); 40]; // 0.1: a value whose naive mean does NOT round back
    for est in all(5) {
        let out = est.series(&bars);
        let warm = est.lookback();
        assert!(out[..warm].iter().all(|v| v.is_nan()), "{}: NaN before {warm}", est.name());
        for (i, v) in out.iter().enumerate().skip(warm) {
            assert_bits(&format!("{} bar {i}", est.name()), *v, 0.0);
        }
    }
}

/// A price that DOUBLES every bar, opening at the previous close and trading only between open
/// and close: every log return is exactly `ln 2`, so the path has no randomness at all.
///
/// The textbook relations, each the reason an estimator exists: the demeaned and drift-free ones
/// (close-to-close, Rogers–Satchell, Yang–Zhang) read EXACTLY zero — close-to-close through the
/// constant-window rule, the other two because `ln(H/C)` and `ln(L/O)` vanish — while the
/// zero-drift ones mistake the drift for volatility: Parkinson reads `√(ln 2) / 2`, Garman–Klass
/// `ln 2 · √(1.5 − 2·ln 2)` and the zero-mean EWMA `ln 2`.
#[test]
fn a_pure_drift_series_separates_the_drift_robust_estimators_from_the_rest() {
    let mut o = 100.0f64;
    let mut bars = Vec::new();
    for _ in 0..30 {
        bars.push(bar(o, 2.0 * o, o, 2.0 * o)); // doubling is exact, so every ratio is exactly 2
        o *= 2.0;
    }
    let w = 5;
    let last = |est: Estimator| *est.series(&bars).last().unwrap();
    for est in [
        Estimator::CloseToClose { window: w },
        Estimator::RogersSatchell { window: w },
        Estimator::YangZhang { window: w },
    ] {
        assert_bits(est.name(), last(est), 0.0);
    }
    let near = |what: &str, got: f64, want: f64| {
        let rel = ((got - want) / want).abs();
        assert!(rel < 1e-14, "{what}: {got} vs textbook {want} (rel {rel:e})");
    };
    near("parkinson", last(Estimator::Parkinson { window: w }), LN_2.sqrt() / 2.0);
    near(
        "garman-klass",
        last(Estimator::GarmanKlass { window: w }),
        LN_2 * (1.5 - 2.0 * LN_2).sqrt(),
    );
    near("ewma", last(Estimator::Ewma { lambda: RISKMETRICS_LAMBDA }), LN_2);
}

#[test]
fn scaling_every_price_by_a_power_of_two_moves_no_bit() {
    // `x * 1024` is exact, so every ratio — and therefore every log — is bit-identical.
    let bars = lcg_bars(120, 3);
    let scaled: Vec<Bar> = bars
        .iter()
        .map(|b| bar(b.open * 1024.0, b.high * 1024.0, b.low * 1024.0, b.close * 1024.0))
        .collect();
    for est in all(10) {
        let (a, b) = (est.series(&bars), est.series(&scaled));
        for (i, (x, y)) in a.iter().zip(&b).enumerate() {
            assert!(x.to_bits() == y.to_bits(), "{} bar {i}: {x:e} vs {y:e}", est.name());
        }
        assert!(a.iter().any(|v| v.is_finite() && *v > 0.0), "{}: a vacuous series", est.name());
    }
}

#[test]
fn scaling_every_price_by_any_factor_moves_the_value_by_rounding_only() {
    let bars = lcg_bars(120, 5);
    let scaled: Vec<Bar> =
        bars.iter().map(|b| bar(b.open * 3.7, b.high * 3.7, b.low * 3.7, b.close * 3.7)).collect();
    for est in all(10) {
        for (i, (x, y)) in est.series(&bars).iter().zip(est.series(&scaled)).enumerate() {
            if x.is_nan() {
                assert!(y.is_nan(), "{} bar {i}: warm-up moved", est.name());
                continue;
            }
            let rel = ((x - y) / x).abs();
            assert!(rel < 1e-9, "{} bar {i}: {x:e} vs {y:e} (rel {rel:e})", est.name());
        }
    }
}

#[test]
fn the_year_basis_scales_by_its_square_root() {
    let v = 0.0123;
    // Quadrupling the basis doubles the annual vol — exactly, since both scalings are by powers of 2.
    assert_bits("4x basis", annualize(v, 4.0 * 252.0), 2.0 * annualize(v, 252.0));
    for basis in [DAYS_PER_YEAR, 252.0, 365.0 * 24.0] {
        let back = deannualize(annualize(v, basis), basis);
        let rel = ((back - v) / v).abs();
        assert!(rel < 1e-15, "basis {basis}: {back} vs {v} (rel {rel:e})");
    }
    // 1%/day on the calendar-day convention is 19.105%/year (√365 ≈ 19.105).
    let ann = annualize(0.01, DAYS_PER_YEAR);
    assert!((ann - 0.191049731745428).abs() < 1e-15, "got {ann}");
}

// ============================ the registry rows ===============================

/// Each estimator's registry row, at the row's default, is the same series as the public API and
/// warms up exactly where `Estimator::lookback` says.
#[test]
fn every_registry_row_is_the_public_estimator_at_its_default() {
    let bars = lcg_bars(200, 11);
    for est in all(20) {
        let ind = make(est.name()).unwrap_or_else(|| panic!("{} is not registered", est.name()));
        assert_eq!(ind.name(), est.name());
        let row = ind.vectorize(&bars).remove(0);
        let api = est.series(&bars);
        for (i, (x, y)) in row.iter().zip(&api).enumerate() {
            assert!(x.to_bits() == y.to_bits(), "{} bar {i}: row {x:e} vs api {y:e}", est.name());
        }
        let first = api.iter().position(|v| !v.is_nan());
        assert_eq!(first, Some(est.lookback()), "{}: first value", est.name());
        assert_eq!(ind.lookback(), est.lookback(), "{}: registry lookback", est.name());
    }
}

#[test]
fn a_registry_row_never_panics_on_a_degenerate_parameter() {
    // The registry clamps into the ParamSpec range; a window below 2 or a lambda outside (0, 1)
    // must never reach `RealizedVol::new`'s panic through it.
    for est in all(20) {
        for raw in [0.0, -3.0, 1.0, 1e9, f64::NAN, f64::INFINITY] {
            let mut ind = make_with(est.name(), &[raw]).unwrap();
            let _ = ind.on_bar(&close(100.0));
        }
    }
}
