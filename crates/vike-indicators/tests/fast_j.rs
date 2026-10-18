//! BBANDS_PCTB, BBANDS_WIDTH, DONCHIAN_WIDTH, ULCER, CHOP and the six realized-volatility rows
//! (RVOL_EWMA, RVOL_CLOSE_TO_CLOSE, RVOL_PARKINSON, RVOL_GARMAN_KLASS, RVOL_ROGERS_SATCHELL,
//! RVOL_YANG_ZHANG): the fast cores against a textbook reference, and the stream (`next`,
//! `RealizedVol::update`), the plain-number entry (`next_value` / `next_hl` / `next_hlc` /
//! `RealizedVol::update_ohlc`), the batch (`vectorize`, `Estimator::series`), the registry and the
//! slice call (`series::*`) against each other, bit for bit.
//!
//! The references do each value the long way (every window folded from scratch, divisions, the
//! two-pass variance, the `Vec` of existing terms). The cores carry running sums and multiply by
//! `1/n`, so the last bits differ; the tolerance is relative 1e-9 of the value.

use vike_indicators::indicators::{BbandsPctb, BbandsWidth, Chop, DonchianWidth, Ulcer};
use vike_indicators::realized_vol::{Estimator, RealizedVol};
use vike_indicators::{Indicator, make_with, series};
use vike_marketdata::Bar;

fn closes(level: f64, step: f64, n: usize) -> Vec<f64> {
    let mut s: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut p = level;
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            p += ((s >> 11) as f64 / (1u64 << 53) as f64 - 0.5) * step;
            p
        })
        .collect()
}

/// A bar around each close, scaled by `spread`: a gap from the previous close to the open, a range
/// that varies, `low <= open, close <= high`, so every price is positive when the closes are.
fn bars_of(c: &[f64], spread: f64) -> Vec<Bar> {
    let mut prev = c.first().copied().unwrap_or(0.0);
    c.iter()
        .enumerate()
        .map(|(i, &x)| {
            let open = prev + spread * 0.2 * ((i % 3) as f64 - 1.0);
            let up = spread * (0.3 + (i % 7) as f64 * 0.1);
            let dn = spread * (0.2 + (i % 5) as f64 * 0.15);
            let high = x.max(open) + up;
            let low = x.min(open) - dn;
            prev = x;
            Bar::ohlcv(i as i64 * 60_000, open, high, low, x, 1.0)
        })
        .collect()
}

fn columns(b: &[Bar]) -> [Vec<f64>; 5] {
    [
        b.iter().map(|x| x.open).collect(),
        b.iter().map(|x| x.high).collect(),
        b.iter().map(|x| x.low).collect(),
        b.iter().map(|x| x.close).collect(),
        b.iter().map(|x| x.volume).collect(),
    ]
}

// ------------------------------------------------------------------ textbook references

/// Bollinger %B and width the long way (band edges not rounded to the price level). A window with a `NaN` is `NaN`; a window of
/// bit-equal closes has zero deviation.
struct BbRef {
    pctb: Vec<f64>,
    width: Vec<f64>,
}

fn bb_ref(c: &[f64], n: usize, k: f64) -> BbRef {
    let len = c.len();
    let nan = || vec![f64::NAN; len];
    let mut r = BbRef { pctb: nan(), width: nan() };
    for i in (n - 1)..len {
        let w = &c[i + 1 - n..=i];
        // Everything off the first value: no deviation is rounded to the price level.
        let ms = w.iter().map(|v| v - w[0]).sum::<f64>() / n as f64;
        let m = w[0] + ms;
        let flat = !w[0].is_nan() && w.iter().all(|v| *v == w[0]);
        let sd = if flat {
            0.0
        } else {
            (w.iter().map(|v| ((v - w[0]) - ms) * ((v - w[0]) - ms)).sum::<f64>() / n as f64).sqrt()
        };
        // The band edges are not rounded to the price level (see `PctbCore`).
        let dev = (c[i] - w[0]) - ms;
        r.pctb[i] = if sd == 0.0 { f64::NAN } else { (dev / sd + k) / (2.0 * k) };
        r.width[i] = if m == 0.0 { f64::NAN } else { 2.0 * k * sd / m };
    }
    r
}

fn donchian_width_ref(h: &[f64], l: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; h.len().min(l.len())];
    for i in (n - 1)..out.len() {
        let hh = h[i + 1 - n..=i].iter().cloned().fold(f64::MIN, f64::max);
        let ll = l[i + 1 - n..=i].iter().cloned().fold(f64::MAX, f64::min);
        out[i] = hh - ll;
    }
    out
}

fn ulcer_ref(c: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; c.len()];
    for i in (n - 1)..c.len() {
        let w = &c[i + 1 - n..=i];
        let peak = w.iter().cloned().fold(f64::MIN, f64::max);
        let sq_sum = if peak != 0.0 {
            w.iter().map(|v| ((v - peak) / peak * 100.0).powi(2)).sum::<f64>()
        } else {
            0.0
        };
        out[i] = (sq_sum / n as f64).sqrt();
    }
    out
}

fn chop_ref(h: &[f64], l: &[f64], c: &[f64], n: usize) -> Vec<f64> {
    let len = h.len().min(l.len()).min(c.len());
    let tr: Vec<f64> = (0..len)
        .map(|i| {
            if i == 0 {
                h[0] - l[0]
            } else {
                (h[i] - l[i]).max((h[i] - c[i - 1]).abs()).max((l[i] - c[i - 1]).abs())
            }
        })
        .collect();
    let mut out = vec![f64::NAN; len];
    for i in (n - 1)..len {
        let sum: f64 = tr[i + 1 - n..=i].iter().sum();
        let hh = h[i + 1 - n..=i].iter().cloned().fold(f64::MIN, f64::max);
        let ll = l[i + 1 - n..=i].iter().cloned().fold(f64::MAX, f64::min);
        let rng = hh - ll;
        if rng > 0.0 && sum > 0.0 {
            out[i] = 100.0 * libm::log10(sum / rng) / libm::log10(n as f64);
        }
    }
    out
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Ewma,
    CloseToClose,
    Parkinson,
    GarmanKlass,
    RogersSatchell,
    YangZhang,
}

fn usable(p: f64) -> bool {
    p.is_finite() && p > 0.0
}

fn mean_of(w: &[f64]) -> f64 {
    let mut s = 0.0;
    for &x in w {
        s += x;
    }
    s / w.len() as f64
}

fn sample_var(w: &[f64]) -> f64 {
    if w[1..].iter().all(|v| *v == w[0]) {
        return 0.0;
    }
    let m = mean_of(w);
    let mut ss = 0.0;
    for &x in w {
        ss += (x - m) * (x - m);
    }
    ss / (w.len() as f64 - 1.0)
}

fn push_bounded(w: &mut Vec<f64>, x: f64, n: usize) {
    if w.len() == n {
        w.remove(0);
    }
    w.push(x);
}

/// The estimators the long way: a `Vec` of the last `n` EXISTING terms, folded from scratch.
fn rvol_ref(kind: Kind, n: usize, lambda: f64, bars: &[Bar]) -> Vec<f64> {
    let mut prev: Option<f64> = None;
    let (mut xs, mut ys, mut zs) = (Vec::new(), Vec::new(), Vec::new());
    let mut var: Option<f64> = None;
    let ln2 = std::f64::consts::LN_2;
    bars.iter()
        .map(|b| {
            let p = prev;
            prev = usable(b.close).then_some(b.close);
            let (o, h, l, c) = (b.open, b.high, b.low, b.close);
            let all4 = usable(o) && usable(h) && usable(l) && usable(c);
            let ret = p.filter(|_| usable(c)).map(|p| libm::log(c / p));
            let rs = || {
                let (hc, ho) = (libm::log(h / c), libm::log(h / o));
                let (lc, lo) = (libm::log(l / c), libm::log(l / o));
                hc * ho + lc * lo
            };
            match kind {
                Kind::Ewma => match ret {
                    Some(r) => {
                        let v = match var {
                            None => r * r,
                            Some(v) => lambda * v + (1.0 - lambda) * (r * r),
                        };
                        var = Some(v);
                        v.sqrt()
                    }
                    None => f64::NAN,
                },
                Kind::CloseToClose => match ret {
                    Some(r) => {
                        push_bounded(&mut xs, r, n);
                        if xs.len() == n { sample_var(&xs).sqrt() } else { f64::NAN }
                    }
                    None => f64::NAN,
                },
                Kind::Parkinson | Kind::GarmanKlass | Kind::RogersSatchell => {
                    let term = match kind {
                        Kind::Parkinson => (usable(h) && usable(l)).then(|| {
                            let x = libm::log(h / l);
                            x * x
                        }),
                        Kind::GarmanKlass => all4.then(|| {
                            let (hl, co) = (libm::log(h / l), libm::log(c / o));
                            0.5 * (hl * hl) - (2.0 * ln2 - 1.0) * (co * co)
                        }),
                        _ => all4.then(rs),
                    };
                    let scale = if kind == Kind::Parkinson { 1.0 / (4.0 * ln2) } else { 1.0 };
                    match term {
                        Some(t) => {
                            push_bounded(&mut xs, t, n);
                            if xs.len() == n { (scale * mean_of(&xs)).sqrt() } else { f64::NAN }
                        }
                        None => f64::NAN,
                    }
                }
                Kind::YangZhang => match (p, all4) {
                    (Some(p), true) => {
                        push_bounded(&mut xs, libm::log(o / p), n);
                        push_bounded(&mut ys, libm::log(c / o), n);
                        push_bounded(&mut zs, rs(), n);
                        if xs.len() == n {
                            let nf = n as f64;
                            let k = 0.34 / (1.34 + (nf + 1.0) / (nf - 1.0));
                            (sample_var(&xs) + k * sample_var(&ys) + (1.0 - k) * mean_of(&zs))
                                .sqrt()
                        } else {
                            f64::NAN
                        }
                    }
                    _ => f64::NAN,
                },
            }
        })
        .collect()
}

fn estimator(kind: Kind, n: usize, lambda: f64) -> Estimator {
    match kind {
        Kind::Ewma => Estimator::Ewma { lambda },
        Kind::CloseToClose => Estimator::CloseToClose { window: n },
        Kind::Parkinson => Estimator::Parkinson { window: n },
        Kind::GarmanKlass => Estimator::GarmanKlass { window: n },
        Kind::RogersSatchell => Estimator::RogersSatchell { window: n },
        Kind::YangZhang => Estimator::YangZhang { window: n },
    }
}

const KINDS: [(Kind, &str); 6] = [
    (Kind::Ewma, "rvol_ewma"),
    (Kind::CloseToClose, "rvol_close_to_close"),
    (Kind::Parkinson, "rvol_parkinson"),
    (Kind::GarmanKlass, "rvol_garman_klass"),
    (Kind::RogersSatchell, "rvol_rogers_satchell"),
    (Kind::YangZhang, "rvol_yang_zhang"),
];

/// The slice call for `kind`, through `series::*`.
fn rvol_series(kind: Kind, cols: &[Vec<f64>; 5], n: usize, lambda: f64) -> Vec<f64> {
    let [o, h, l, c, _] = cols;
    let w = n as f64;
    match kind {
        Kind::Ewma => series::rvol_ewma(c, lambda),
        Kind::CloseToClose => series::rvol_close_to_close(c, w),
        Kind::Parkinson => series::rvol_parkinson(h, l, w),
        Kind::GarmanKlass => series::rvol_garman_klass(o, h, l, c, w),
        Kind::RogersSatchell => series::rvol_rogers_satchell(o, h, l, c, w),
        Kind::YangZhang => series::rvol_yang_zhang(o, h, l, c, w),
    }
    .unwrap()
}

// ------------------------------------------------------------------ comparison helpers

/// The worst relative error; NaN positions must agree, and a reference zero must be exact. `allow(i)`
/// is the extra relative allowance at position `i`.
fn max_rel_error_with(
    name: &str,
    got: &[f64],
    want: &[f64],
    rel: f64,
    allow: impl Fn(usize) -> f64,
) -> f64 {
    assert_eq!(got.len(), want.len(), "{name}: length");
    let mut worst = 0.0_f64;
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        if w.is_nan() {
            assert!(g.is_nan(), "{name}[{i}]: want NaN, got {g}");
        } else {
            let err = (g - w).abs() / w.abs().max(1e-300);
            assert!(
                err <= rel + allow(i),
                "{name}[{i}]: got {g}, want {w}, relative error {err:e}"
            );
            worst = worst.max(err);
        }
    }
    worst
}

/// [`max_rel_error_with`] for a value that passes through zero (`%B`): the error is measured against
/// `max(|want|, 1)`.
fn max_scaled_error_with(
    name: &str,
    got: &[f64],
    want: &[f64],
    rel: f64,
    allow: impl Fn(usize) -> f64,
) -> f64 {
    assert_eq!(got.len(), want.len(), "{name}: length");
    let mut worst = 0.0_f64;
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        if w.is_nan() {
            assert!(g.is_nan(), "{name}[{i}]: want NaN, got {g}");
        } else {
            let err = (g - w).abs() / w.abs().max(1.0);
            assert!(err <= rel + allow(i), "{name}[{i}]: got {g}, want {w}, scaled error {err:e}");
            worst = worst.max(err);
        }
    }
    worst
}

fn max_rel_error(name: &str, got: &[f64], want: &[f64], rel: f64) -> f64 {
    max_rel_error_with(name, got, want, rel, |_| 0.0)
}

fn same(a: &[f64], b: &[f64]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan()))
}

const SCALES: [(f64, f64); 3] = [(100.0, 2.0), (1.0e6, 0.01), (1.0e-3, 1.0e-6)];
const PERIODS: [usize; 6] = [2, 3, 5, 20, 64, 200];
const WINDOWS: [usize; 8] = [2, 3, 4, 5, 20, 64, 200, 400];
const LAMBDAS: [f64; 3] = [0.5, 0.94, 0.999];

fn with_nan(mut c: Vec<f64>, at: usize) -> Vec<f64> {
    c[at] = f64::NAN;
    c
}

// ------------------------------------------------------------------ against the textbook

#[test]
fn bollinger_percent_b_and_width_match_a_window_folded_from_scratch() {
    for (level, step) in SCALES {
        let c = closes(level, step, 3000);
        for n in PERIODS {
            for k in [0.5, 2.0, 5.0] {
                for (tag, c) in [("", c.clone()), (" NaN", with_nan(c.clone(), 1500))] {
                    let r = bb_ref(&c, n, k);
                    let rel = 1e-9;
                    let pctb = series::bbands_pctb(&c, n as f64, k).unwrap();
                    let width = series::bbands_width(&c, n as f64, k).unwrap();
                    // %B is O(1) and passes through zero, so its error is measured against max(|want|, 1).
                    max_scaled_error_with(
                        &format!("pctb({n},{k}){tag} level {level}"),
                        &pctb,
                        &r.pctb,
                        rel,
                        |_| 0.0,
                    );
                    max_rel_error_with(
                        &format!("width({n},{k}){tag} level {level}"),
                        &width,
                        &r.width,
                        rel,
                        |_| 0.0,
                    );
                }
            }
        }
    }
}

#[test]
fn a_flat_window_has_no_band_whatever_came_before() {
    for flat in [100.0, 0.1, 123456.789] {
        let mut c = closes(1.0e6, 0.5, 700);
        c.extend(std::iter::repeat_n(flat, 100));
        for n in [2usize, 20, 200] {
            let pctb = series::bbands_pctb(&c, n as f64, 2.0).unwrap();
            let width = series::bbands_width(&c, n as f64, 2.0).unwrap();
            for i in (700 + n - 1)..c.len() {
                assert!(pctb[i].is_nan(), "pctb({n}) at {i} over a window flat at {flat}");
                assert_eq!(width[i], 0.0, "width({n}) at {i} over a window flat at {flat}");
            }
        }
    }
}

#[test]
fn a_variance_that_is_small_but_real_still_has_a_band() {
    let mut c = vec![100.0; 30];
    c.extend((0..100).map(|i| if i % 2 == 0 { 100.0 } else { 100.000001 }));
    for n in [2usize, 5, 20] {
        let pctb = series::bbands_pctb(&c, n as f64, 2.0).unwrap();
        let width = series::bbands_width(&c, n as f64, 2.0).unwrap();
        for i in (30 + n)..c.len() {
            assert!(pctb[i].is_finite(), "pctb({n}) at {i}");
            assert!(width[i] > 0.0 && width[i].is_finite(), "width({n}) at {i}");
        }
    }
}

#[test]
fn donchian_width_is_the_same_fold() {
    for (level, step) in SCALES {
        let c = closes(level, step, 3000);
        for tag in [false, true] {
            let c = if tag { with_nan(c.clone(), 1500) } else { c.clone() };
            let b = bars_of(&c, step);
            let [_, h, l, _, _] = columns(&b);
            for n in PERIODS {
                let got = series::donchian_width(&h, &l, n as f64).unwrap();
                assert!(
                    same(&got, &donchian_width_ref(&h, &l, n)),
                    "donchian_width({n}) level {level} nan {tag}: it is the same subtraction of the same extremes"
                );
            }
        }
    }
}

#[test]
fn ulcer_matches_a_window_folded_from_scratch() {
    for (level, step) in SCALES {
        let c = closes(level, step, 4000);
        for n in PERIODS {
            max_rel_error(
                &format!("ulcer({n}) level {level}"),
                &series::ulcer(&c, n as f64).unwrap(),
                &ulcer_ref(&c, n),
                1e-9,
            );
            let cn = with_nan(c.clone(), 2000);
            max_rel_error(
                &format!("ulcer({n}) NaN level {level}"),
                &series::ulcer(&cn, n as f64).unwrap(),
                &ulcer_ref(&cn, n),
                1e-9,
            );
        }
    }
}

#[test]
fn ulcer_of_a_flat_window_is_exactly_zero_whatever_came_before() {
    for flat in [100.0, 0.1, 123456.789] {
        let mut c = closes(1.0e6, 0.5, 700);
        c.extend(std::iter::repeat_n(flat, 100));
        for n in [2usize, 20, 200] {
            let got = series::ulcer(&c, n as f64).unwrap();
            for (i, g) in got.iter().enumerate().skip(700 + n - 1) {
                assert_eq!(*g, 0.0, "ulcer({n}) at {i} over a window flat at {flat}");
            }
        }
    }
}

#[test]
fn ulcer_follows_a_crash_and_its_recovery() {
    // A price that halves and recovers, over and over: the peak leaves the window, the window's
    // drawdowns change by orders of magnitude, and the carried moments must keep up.
    let mut c = Vec::new();
    for round in 0..40 {
        for i in 0..60 {
            let depth = if round % 2 == 0 { 0.5 } else { 1.0e-4 };
            c.push(100.0 * (1.0 - depth * (i as f64 / 60.0)));
        }
    }
    for n in [2usize, 5, 14, 60, 200] {
        max_rel_error(
            &format!("ulcer({n}) crash"),
            &series::ulcer(&c, n as f64).unwrap(),
            &ulcer_ref(&c, n),
            1e-9,
        );
    }
}

#[test]
fn chop_matches_a_window_folded_from_scratch() {
    for (level, step) in SCALES {
        let c = closes(level, step, 4000);
        for n in PERIODS {
            for tag in [false, true] {
                let c = if tag { with_nan(c.clone(), 2000) } else { c.clone() };
                let b = bars_of(&c, step);
                let [_, h, l, cl, _] = columns(&b);
                max_rel_error(
                    &format!("chop({n}) level {level} nan {tag}"),
                    &series::chop(&h, &l, &cl, n as f64).unwrap(),
                    &chop_ref(&h, &l, &cl, n),
                    1e-9,
                );
            }
        }
    }
}

#[test]
fn chop_of_a_flat_market_is_nan_and_recovers() {
    let c = closes(100.0, 2.0, 300);
    let mut b = bars_of(&c, 1.0);
    let flat = Bar::ohlcv(0, 100.0, 100.0, 100.0, 100.0, 1.0);
    b.extend(std::iter::repeat_n(flat, 50));
    b.extend(bars_of(&closes(100.0, 2.0, 300), 1.0));
    let [_, h, l, cl, _] = columns(&b);
    for n in [2usize, 14, 20] {
        max_rel_error(
            &format!("chop({n}) through a flat stretch"),
            &series::chop(&h, &l, &cl, n as f64).unwrap(),
            &chop_ref(&h, &l, &cl, n),
            1e-9,
        );
    }
}

#[test]
fn realized_volatility_matches_the_estimators_folded_from_scratch() {
    for (level, step) in SCALES {
        let c = closes(level, step, 3000);
        for tag in [false, true] {
            let c = if tag { with_nan(c.clone(), 1500) } else { c.clone() };
            let b = bars_of(&c, step * 0.5);
            let cols = columns(&b);
            for (kind, name) in KINDS {
                let windows: &[usize] = if kind == Kind::Ewma { &[2] } else { &WINDOWS };
                let lambdas: &[f64] = if kind == Kind::Ewma { &LAMBDAS } else { &[0.94] };
                for &n in windows {
                    for &lambda in lambdas {
                        // Rogers-Satchell reads `ln(H/C)` as `ln(H/O) - ln(C/O)` (three logs, not four):
                        // exact to the logs' rounding, but at level 1e6 a move of a hundredth is 1e-8 of the
                        // price, each log of a ratio carries the ratio's half ulp of 1 (1e-16, i.e. 1e-8 of
                        // the move) and the two forms round differently. Prices on that grid cannot resolve
                        // the move to better than 1e-4 anyway; the other estimators keep 1e-9.
                        let difference_form =
                            matches!(kind, Kind::RogersSatchell | Kind::YangZhang);
                        let rel = if difference_form && level >= 1.0e5 { 1e-6 } else { 1e-9 };
                        max_rel_error(
                            &format!("{name}({n},{lambda}) level {level} nan {tag}"),
                            &rvol_series(kind, &cols, n, lambda),
                            &rvol_ref(kind, n, lambda, &b),
                            rel,
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn unusable_prices_remove_terms_and_leave_the_state_alone() {
    let c = closes(100.0, 2.0, 1500);
    let mut b = bars_of(&c, 1.0);
    b[400].high = f64::INFINITY;
    b[410].low = 0.0;
    b[420].open = -1.0;
    b[430].close = f64::NEG_INFINITY;
    b[700].close = 0.0;
    b[701].open = f64::NAN;
    let cols = columns(&b);
    for (kind, name) in KINDS {
        for n in [2usize, 5, 20, 64] {
            max_rel_error(
                &format!("{name}({n}) with unusable prices"),
                &rvol_series(kind, &cols, n, 0.94),
                &rvol_ref(kind, n, 0.94, &b),
                1e-9,
            );
        }
    }
}

#[test]
fn a_window_of_no_movement_is_exactly_zero_whatever_came_before() {
    let mut b = bars_of(&closes(100.0, 2.0, 700), 1.0);
    let flat = Bar::ohlcv(0, 100.0, 100.0, 100.0, 100.0, 1.0);
    b.extend(std::iter::repeat_n(flat, 150));
    let cols = columns(&b);
    for (kind, name) in KINDS {
        if kind == Kind::Ewma {
            continue; // infinite memory: it decays, it does not reach zero
        }
        for n in [2usize, 20, 64] {
            let got = rvol_series(kind, &cols, n, 0.94);
            // The first flat bar still has a return against the volatile bar before it; after `n`
            // flat bars every term in the window is exactly zero.
            for (i, g) in got.iter().enumerate().skip(700 + n + 1) {
                assert_eq!(*g, 0.0, "{name}({n}) at {i}");
            }
        }
    }
}

#[test]
fn the_first_value_is_where_the_estimators_say() {
    let c = closes(100.0, 2.0, 700);
    let b = bars_of(&c, 1.0);
    let cols = columns(&b);
    let first = |v: &[f64]| v.iter().position(|x| !x.is_nan());
    for n in [4usize, 5, 20, 64, 200] {
        let nf = n as f64;
        let [_, h, l, cl, _] = &cols;
        assert_eq!(first(&series::bbands_pctb(cl, nf, 2.0).unwrap()), Some(n - 1));
        assert_eq!(first(&series::bbands_width(cl, nf, 2.0).unwrap()), Some(n - 1));
        assert_eq!(first(&series::donchian_width(h, l, nf).unwrap()), Some(n - 1));
        assert_eq!(first(&series::ulcer(cl, nf).unwrap()), Some(n - 1));
        assert_eq!(first(&series::chop(h, l, cl, nf).unwrap()), Some(n - 1));
        assert_eq!(BbandsPctb::new(nf, 2.0).unwrap().lookback(), n - 1);
        assert_eq!(BbandsWidth::new(nf, 2.0).unwrap().lookback(), n - 1);
        assert_eq!(DonchianWidth::new(nf).unwrap().lookback(), n - 1);
        assert_eq!(Ulcer::new(nf).unwrap().lookback(), n - 1);
        assert_eq!(Chop::new(nf).unwrap().lookback(), n - 1);
    }
    for (kind, name) in KINDS {
        for n in [2usize, 5, 20, 64, 400] {
            let est = estimator(kind, n, 0.94);
            assert_eq!(
                first(&rvol_series(kind, &cols, n, 0.94)),
                Some(est.lookback()),
                "{name}({n})"
            );
        }
    }
}

// ------------------------------------------------------------------ every way in, the same bits

fn lengths(n: usize) -> [usize; 8] {
    [0, 1, n.saturating_sub(1), n, n + 1, 3 * n, 3 * n + 1, 2600]
}

fn series_for(len: usize) -> Vec<f64> {
    let mut c = closes(100.0, 2.0, len);
    if len > 40 {
        c[len / 3] = f64::NAN;
    }
    c
}

fn run<I: Indicator + ?Sized>(ind: &mut I, b: &[Bar]) -> Vec<f64> {
    b.iter().map(|x| ind.on_bar(x)[0]).collect()
}

#[test]
fn every_way_in_gives_the_same_bits_for_the_bands_the_extremes_and_the_ranges() {
    for n in [2usize, 3, 7, 20, 64, 200] {
        for len in lengths(n) {
            let c = series_for(len);
            let b = bars_of(&c, 1.0);
            let [_, h, l, cl, _] = columns(&b);
            let nf = n as f64;

            for k in [0.5, 2.0] {
                let mut ind = BbandsPctb::new(nf, k).unwrap();
                let stream: Vec<f64> = b.iter().map(|x| ind.next(x)).collect();
                let mut ind = BbandsPctb::new(nf, k).unwrap();
                let value: Vec<f64> = cl.iter().map(|&x| ind.next_value(x)).collect();
                assert!(same(&stream, &value), "pctb n={n} k={k} len={len}: next vs next_value");
                assert!(
                    same(&stream, &BbandsPctb::new(nf, k).unwrap().vectorize(&b)[0]),
                    "pctb n={n} len={len}: vectorize"
                );
                assert!(
                    same(&stream, &series::bbands_pctb(&cl, nf, k).unwrap()),
                    "pctb n={n} len={len}: series"
                );
                let reg = make_with("bbands_pctb", &[nf, k]).unwrap();
                assert!(same(&stream, &reg.vectorize(&b)[0]), "pctb n={n} len={len}: registry");

                let mut ind = BbandsWidth::new(nf, k).unwrap();
                let stream: Vec<f64> = b.iter().map(|x| ind.next(x)).collect();
                let mut ind = BbandsWidth::new(nf, k).unwrap();
                let value: Vec<f64> = cl.iter().map(|&x| ind.next_value(x)).collect();
                assert!(same(&stream, &value), "width n={n} k={k} len={len}: next vs next_value");
                assert!(
                    same(&stream, &BbandsWidth::new(nf, k).unwrap().vectorize(&b)[0]),
                    "width n={n} len={len}: vectorize"
                );
                assert!(
                    same(&stream, &series::bbands_width(&cl, nf, k).unwrap()),
                    "width n={n} len={len}: series"
                );
                let reg = make_with("bbands_width", &[nf, k]).unwrap();
                assert!(same(&stream, &reg.vectorize(&b)[0]), "width n={n} len={len}: registry");
            }

            let mut ind = DonchianWidth::new(nf).unwrap();
            let stream: Vec<f64> = b.iter().map(|x| ind.next(x)).collect();
            let mut ind = DonchianWidth::new(nf).unwrap();
            let value: Vec<f64> = b.iter().map(|x| ind.next_hl(x.high, x.low)).collect();
            assert!(same(&stream, &value), "donchian_width n={n} len={len}: next vs next_hl");
            assert!(
                same(&stream, &DonchianWidth::new(nf).unwrap().vectorize(&b)[0]),
                "donchian_width n={n} len={len}: vectorize"
            );
            assert!(
                same(&stream, &series::donchian_width(&h, &l, nf).unwrap()),
                "donchian_width n={n} len={len}: series"
            );
            let reg = make_with("donchian_width", &[nf]).unwrap();
            assert!(
                same(&stream, &reg.vectorize(&b)[0]),
                "donchian_width n={n} len={len}: registry"
            );

            let mut ind = Ulcer::new(nf).unwrap();
            let stream: Vec<f64> = b.iter().map(|x| ind.next(x)).collect();
            let mut ind = Ulcer::new(nf).unwrap();
            let value: Vec<f64> = cl.iter().map(|&x| ind.next_value(x)).collect();
            assert!(same(&stream, &value), "ulcer n={n} len={len}: next vs next_value");
            assert!(
                same(&stream, &Ulcer::new(nf).unwrap().vectorize(&b)[0]),
                "ulcer n={n} len={len}: vectorize"
            );
            assert!(
                same(&stream, &series::ulcer(&cl, nf).unwrap()),
                "ulcer n={n} len={len}: series"
            );
            let reg = make_with("ulcer", &[nf]).unwrap();
            assert!(same(&stream, &reg.vectorize(&b)[0]), "ulcer n={n} len={len}: registry");

            let mut ind = Chop::new(nf).unwrap();
            let stream: Vec<f64> = b.iter().map(|x| ind.next(x)).collect();
            let mut ind = Chop::new(nf).unwrap();
            let value: Vec<f64> = b.iter().map(|x| ind.next_hlc(x.high, x.low, x.close)).collect();
            assert!(same(&stream, &value), "chop n={n} len={len}: next vs next_hlc");
            assert!(
                same(&stream, &Chop::new(nf).unwrap().vectorize(&b)[0]),
                "chop n={n} len={len}: vectorize"
            );
            assert!(
                same(&stream, &series::chop(&h, &l, &cl, nf).unwrap()),
                "chop n={n} len={len}: series"
            );
            let reg = make_with("chop", &[nf]).unwrap();
            assert!(same(&stream, &reg.vectorize(&b)[0]), "chop n={n} len={len}: registry");
        }
    }
}

#[test]
fn every_way_in_gives_the_same_bits_for_the_realized_volatilities() {
    for n in [2usize, 3, 7, 20, 64, 200, 400] {
        for len in lengths(n) {
            let c = series_for(len);
            let b = bars_of(&c, 1.0);
            let cols = columns(&b);
            for (kind, name) in KINDS {
                let lambdas: &[f64] = if kind == Kind::Ewma { &LAMBDAS } else { &[0.94] };
                if kind == Kind::Ewma && n != 7 {
                    continue; // the EWMA has no window
                }
                for &lambda in lambdas {
                    let est = estimator(kind, n, lambda);
                    let mut rv = RealizedVol::new(est);
                    let stream: Vec<f64> = b.iter().map(|x| rv.update(x)).collect();
                    let mut rv = RealizedVol::new(est);
                    let plain: Vec<f64> =
                        b.iter().map(|x| rv.update_ohlc(x.open, x.high, x.low, x.close)).collect();
                    assert!(same(&stream, &plain), "{name} n={n} len={len}: update vs update_ohlc");
                    assert!(
                        same(&stream, &est.series(&b)),
                        "{name} n={n} len={len}: Estimator::series"
                    );
                    assert!(
                        same(&stream, &rvol_series(kind, &cols, n, lambda)),
                        "{name} n={n} len={len}: series::*"
                    );
                    let raw = if kind == Kind::Ewma { lambda } else { n as f64 };
                    let reg = make_with(name, &[raw]).unwrap();
                    assert!(
                        same(&stream, &reg.vectorize(&b)[0]),
                        "{name} n={n} len={len}: registry"
                    );
                    let mut reg = make_with(name, &[raw]).unwrap();
                    assert!(
                        same(&stream, &run(&mut *reg, &b)),
                        "{name} n={n} len={len}: registry on_bar"
                    );
                }
            }
        }
    }
}

#[test]
fn reset_returns_to_the_first_bar() {
    let c = closes(100.0, 2.0, 700);
    let b = bars_of(&c, 1.0);
    let mut p = BbandsPctb::default();
    let first = run(&mut p, &b);
    p.reset();
    assert!(same(&first, &run(&mut p, &b)), "bbands_pctb");
    let mut w = BbandsWidth::default();
    let first = run(&mut w, &b);
    w.reset();
    assert!(same(&first, &run(&mut w, &b)), "bbands_width");
    let mut d = DonchianWidth::default();
    let first = run(&mut d, &b);
    d.reset();
    assert!(same(&first, &run(&mut d, &b)), "donchian_width");
    let mut u = Ulcer::default();
    let first = run(&mut u, &b);
    u.reset();
    assert!(same(&first, &run(&mut u, &b)), "ulcer");
    let mut ch = Chop::default();
    let first = run(&mut ch, &b);
    ch.reset();
    assert!(same(&first, &run(&mut ch, &b)), "chop");
    for (kind, name) in KINDS {
        let mut rv = RealizedVol::new(estimator(kind, 20, 0.94));
        let first: Vec<f64> = b.iter().map(|x| rv.update(x)).collect();
        rv.reset();
        let again: Vec<f64> = b.iter().map(|x| rv.update(x)).collect();
        assert!(same(&first, &again), "{name}");
    }
}

/// The numbers the report quotes: the worst relative error against the textbook, per indicator,
/// over every scale, window and NaN position this file uses.
///
/// cargo test -p vike-indicators --release --test fast_j -- --ignored --nocapture
#[test]
#[ignore = "prints the measured worst errors"]
fn report_worst_errors() {
    let mut worst = std::collections::BTreeMap::<String, f64>::new();
    let mut note = |k: &str, v: f64| {
        let e = worst.entry(k.to_string()).or_insert(0.0);
        *e = e.max(v);
    };
    for (level, step) in SCALES {
        let c = closes(level, step, 4000);
        let b = bars_of(&c, step * 0.5);
        let cols = columns(&b);
        let [_, h, l, cl, _] = &cols;
        for n in PERIODS {
            for k in [0.5, 2.0, 5.0] {
                let r = bb_ref(&c, n, k);
                let pctb = series::bbands_pctb(&c, n as f64, k).unwrap();
                let width = series::bbands_width(&c, n as f64, k).unwrap();
                let tag = if n < 5 { "n<5" } else { "n>=5" };
                note(
                    &format!("bbands_pctb {tag}"),
                    max_scaled_error_with("pctb", &pctb, &r.pctb, 1.0, |_| 0.0),
                );
                note(&format!("bbands_width {tag}"), max_rel_error("width", &width, &r.width, 1.0));
            }
            note(
                "ulcer",
                max_rel_error(
                    "ulcer",
                    &series::ulcer(&c, n as f64).unwrap(),
                    &ulcer_ref(&c, n),
                    1.0,
                ),
            );
            note(
                "chop",
                max_rel_error(
                    "chop",
                    &series::chop(h, l, cl, n as f64).unwrap(),
                    &chop_ref(h, l, cl, n),
                    1.0,
                ),
            );
        }
        for (kind, name) in KINDS {
            let windows: &[usize] = if kind == Kind::Ewma { &[2] } else { &WINDOWS };
            for &n in windows {
                note(
                    &format!("{name}{}", if level >= 1.0e5 { " (level 1e6)" } else { "" }),
                    max_rel_error(
                        name,
                        &rvol_series(kind, &cols, n, 0.94),
                        &rvol_ref(kind, n, 0.94, &b),
                        1.0,
                    ),
                );
            }
        }
    }
    for (k, v) in &worst {
        println!("worst relative error: {k} {v:e}");
    }
}
