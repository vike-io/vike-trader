//! ALMA, ENVELOPES, MIDPOINT, MIDPRICE, VWMA, ATR_TRAILING_STOP, EVWMA, FRAMA, HILO_ACTIVATOR and
//! VIDYA: the fast cores against a textbook reference, and the stream (`next`), the plain-number
//! entry (`next_value` / `next_hl` / `next_hlc` / `next_cv`), the batch (`vectorize`) and the slice
//! call (`series::*`) against each other, bit for bit.
//!
//! The references do each value the long way and are the kernels the indicators had before the
//! cores (every window folded from scratch, divisions, the SMA seeds). The cores carry running sums,
//! multiply by `1/n` and sum the ALMA window in lanes, so the last bits differ; the tolerance is
//! relative 1e-9 of the value. `midpoint` and `midprice` are exact selections, so they are held to
//! the same bits.

use vike_indicators::indicators::{
    Alma, AtrTrailingStop, Envelopes, Evwma, Frama, HiloActivator, Midpoint, Midprice, Vidya, Vwma,
};
use vike_indicators::{Indicator, series};
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

/// The volume of bar `i`: positive and varied, so a volume weighting is not a plain mean.
fn volume_at(i: usize) -> f64 {
    1.0 + ((i * 7) % 13) as f64 * 50.0
}

/// A bar around each close with a spread that varies (in proportion to `scale`, the size of the
/// series' steps).
fn bars_of(c: &[f64], scale: f64) -> Vec<Bar> {
    c.iter()
        .enumerate()
        .map(|(i, &x)| {
            let up = (0.15 + (i % 7) as f64 * 0.05) * scale;
            let dn = (0.1 + (i % 5) as f64 * 0.075) * scale;
            Bar::ohlcv(i as i64 * 60_000, x, x + up, x - dn, x, volume_at(i))
        })
        .collect()
}

fn highs(b: &[Bar]) -> Vec<f64> {
    b.iter().map(|x| x.high).collect()
}
fn lows(b: &[Bar]) -> Vec<f64> {
    b.iter().map(|x| x.low).collect()
}
fn vols(b: &[Bar]) -> Vec<f64> {
    b.iter().map(|x| x.volume).collect()
}

// ------------------------------------------------------------------ textbook references

fn mean_of(w: &[f64]) -> f64 {
    w.iter().sum::<f64>() / w.len() as f64
}

fn alma_ref(c: &[f64], n: usize, offset: f64, sigma: f64) -> Vec<f64> {
    let m = offset * (n as f64 - 1.0);
    let s = n as f64 / sigma;
    let raw: Vec<f64> =
        (0..n).map(|k| (-((k as f64 - m) * (k as f64 - m)) / (2.0 * s * s)).exp()).collect();
    let total: f64 = raw.iter().sum();
    let w: Vec<f64> = raw.iter().map(|x| x / total).collect();
    (0..c.len())
        .map(|i| {
            if i + 1 < n {
                f64::NAN
            } else {
                (0..n).fold(0.0, |acc, k| acc + w[k] * c[i + 1 - n + k])
            }
        })
        .collect()
}

fn envelopes_ref(c: &[f64], n: usize, pct: f64) -> [Vec<f64>; 3] {
    let mid: Vec<f64> = (0..c.len())
        .map(|i| if i + 1 < n { f64::NAN } else { mean_of(&c[i + 1 - n..=i]) })
        .collect();
    let f = pct / 100.0;
    [
        mid.iter().map(|m| m * (1.0 + f)).collect(),
        mid.clone(),
        mid.iter().map(|m| m * (1.0 - f)).collect(),
    ]
}

fn window_max(w: &[f64]) -> f64 {
    w.iter().cloned().fold(f64::MIN, f64::max)
}
fn window_min(w: &[f64]) -> f64 {
    w.iter().cloned().fold(f64::MAX, f64::min)
}

fn midprice_ref(h: &[f64], l: &[f64], n: usize) -> Vec<f64> {
    (0..h.len().min(l.len()))
        .map(|i| {
            if i + 1 < n {
                f64::NAN
            } else {
                (window_max(&h[i + 1 - n..=i]) + window_min(&l[i + 1 - n..=i])) / 2.0
            }
        })
        .collect()
}

fn vwma_ref(c: &[f64], v: &[f64], n: usize) -> Vec<f64> {
    (0..c.len().min(v.len()))
        .map(|i| {
            if i + 1 < n {
                return f64::NAN;
            }
            let (lo, hi) = (i + 1 - n, i);
            let pv: f64 = (lo..=hi).map(|j| c[j] * v[j]).sum();
            let vv: f64 = v[lo..=hi].iter().sum();
            if vv != 0.0 { pv / vv } else { f64::NAN }
        })
        .collect()
}

fn evwma_ref(c: &[f64], v: &[f64], n: usize) -> Vec<f64> {
    let len = c.len().min(v.len());
    let mut out = vec![f64::NAN; len];
    let (mut prev, mut seeded) = (f64::NAN, false);
    for i in 0..len {
        if i + 1 < n {
            continue;
        }
        if !seeded {
            seeded = true;
            prev = c[i];
        } else {
            let total: f64 = v[i + 1 - n..=i].iter().sum();
            if total > 0.0 {
                prev = ((total - v[i]) * prev + v[i] * c[i]) / total;
            }
        }
        out[i] = prev;
    }
    out
}

fn vidya_ref(c: &[f64], period: usize, cmo: usize) -> Vec<f64> {
    let alpha = 2.0 / (period as f64 + 1.0);
    let mut out = vec![f64::NAN; c.len()];
    let mut prev = f64::NAN;
    for i in cmo..c.len() {
        if i == cmo {
            prev = c[i - 1];
        }
        let (mut up, mut down) = (0.0, 0.0);
        for j in i + 1 - cmo..=i {
            let d = c[j] - c[j - 1];
            if d > 0.0 {
                up += d;
            } else if d < 0.0 {
                down -= d;
            }
        }
        let total = up + down;
        let index = if total == 0.0 { 0.0 } else { ((up - down) / total).abs() };
        let k = alpha * index;
        prev = k * c[i] + (1.0 - k) * prev;
        out[i] = prev;
    }
    out
}

fn frama_ref(h: &[f64], l: &[f64], c: &[f64], period: usize) -> Vec<f64> {
    let half = (period / 2).max(1);
    let n = 2 * half;
    let len = h.len().min(l.len()).min(c.len());
    let ln2 = 2.0_f64.ln();
    let mut out = vec![f64::NAN; len];
    let mut prev = f64::NAN;
    for i in n - 1..len {
        if i == n - 1 {
            prev = c[i];
            out[i] = prev;
            continue;
        }
        let (hw, lw) = (&h[i + 1 - n..=i], &l[i + 1 - n..=i]);
        let (hh1, hh2) = (window_max(&hw[..half]), window_max(&hw[half..]));
        let (ll1, ll2) = (window_min(&lw[..half]), window_min(&lw[half..]));
        let n1 = (hh1 - ll1) / half as f64;
        let n2 = (hh2 - ll2) / half as f64;
        let n3 = (hh1.max(hh2) - ll1.min(ll2)) / n as f64;
        let alpha = if n3 <= 0.0 || n1 + n2 <= 0.0 {
            1.0
        } else {
            let d = ((n1 + n2).ln() - n3.ln()) / ln2;
            (-4.6 * (d - 1.0)).exp().clamp(0.01, 1.0)
        };
        prev = alpha * c[i] + (1.0 - alpha) * prev;
        out[i] = prev;
    }
    out
}

fn hilo_ref(h: &[f64], l: &[f64], c: &[f64], n: usize) -> Vec<f64> {
    let len = h.len().min(l.len()).min(c.len());
    let mut out = vec![f64::NAN; len];
    let (mut ph, mut pl, mut trend) = (f64::NAN, f64::NAN, 0.0);
    for i in n.saturating_sub(1)..len {
        if i + 1 < n {
            continue;
        }
        let (ha, la) = (mean_of(&h[i + 1 - n..=i]), mean_of(&l[i + 1 - n..=i]));
        if trend == 0.0 {
            trend = if c[i] >= (ha + la) / 2.0 { 1.0 } else { -1.0 };
        } else if c[i] > ph {
            trend = 1.0;
        } else if c[i] < pl {
            trend = -1.0;
        }
        ph = ha;
        pl = la;
        out[i] = if trend > 0.0 { la } else { ha };
    }
    out
}

fn atr_stop_ref(h: &[f64], l: &[f64], c: &[f64], n: usize, mult: f64) -> Vec<f64> {
    let len = h.len().min(l.len()).min(c.len());
    let mut out = vec![f64::NAN; len];
    let mut tr = Vec::with_capacity(len);
    let (mut atr, mut stop): (f64, Option<f64>) = (f64::NAN, None);
    for i in 0..len {
        tr.push(if i == 0 {
            h[0] - l[0]
        } else {
            (h[i] - l[i]).max((h[i] - c[i - 1]).abs()).max((l[i] - c[i - 1]).abs())
        });
        if i + 1 < n {
            continue;
        }
        if i + 1 == n {
            atr = tr.iter().sum::<f64>() / n as f64;
        } else {
            atr = (atr * (n as f64 - 1.0) + tr[i]) / n as f64;
        }
        let loss = mult * atr;
        let s = match (stop, i) {
            (Some(prev), 1..) => {
                let pc = c[i - 1];
                if c[i] > prev && pc > prev {
                    prev.max(c[i] - loss)
                } else if c[i] < prev && pc < prev {
                    prev.min(c[i] + loss)
                } else if c[i] > prev {
                    c[i] - loss
                } else {
                    c[i] + loss
                }
            }
            _ => c[i] - loss,
        };
        stop = Some(s);
        out[i] = s;
    }
    out
}

// ------------------------------------------------------------------ comparison helpers

/// Relative error of every value; NaN positions must agree, and a reference zero must be exact.
fn max_rel_error(name: &str, got: &[f64], want: &[f64], rel: f64) -> f64 {
    assert_eq!(got.len(), want.len(), "{name}: length");
    let mut worst = 0.0_f64;
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        if w.is_nan() {
            assert!(g.is_nan(), "{name}[{i}]: want NaN, got {g}");
        } else {
            let err = (g - w).abs() / w.abs().max(1e-300);
            assert!(err <= rel, "{name}[{i}]: got {g}, want {w}, relative error {err:e}");
            worst = worst.max(err);
        }
    }
    worst
}

fn same(a: &[f64], b: &[f64]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan()))
}

const SCALES: [(f64, f64); 3] = [(100.0, 2.0), (1.0e6, 0.01), (1.0e-3, 1.0e-6)];
const WINDOWS: [usize; 8] = [2, 3, 4, 5, 20, 64, 200, 400];

/// A series of bars at one scale, with its columns.
struct Data {
    c: Vec<f64>,
    h: Vec<f64>,
    l: Vec<f64>,
    v: Vec<f64>,
}

fn data(level: f64, step: f64, n: usize, nan_at: Option<usize>) -> Data {
    let mut c = closes(level, step, n);
    if let Some(at) = nan_at {
        c[at] = f64::NAN;
    }
    let b = bars_of(&c, step);
    Data { h: highs(&b), l: lows(&b), v: vols(&b), c }
}

// ------------------------------------------------------------------ against the textbook

#[test]
fn alma_matches_the_gaussian_weighted_mean() {
    for (level, step) in SCALES {
        for nan_at in [None, Some(1500)] {
            let d = data(level, step, 3000, nan_at);
            for n in WINDOWS {
                for (offset, sigma) in [(0.85, 6.0), (0.0, 1.0), (1.0, 20.0), (0.5, 3.0)] {
                    max_rel_error(
                        &format!("alma({n},{offset},{sigma}) level {level} nan {nan_at:?}"),
                        &series::alma(&d.c, n as f64, offset, sigma).unwrap(),
                        &alma_ref(&d.c, n, offset, sigma),
                        1e-9,
                    );
                }
            }
        }
    }
}

#[test]
fn envelopes_are_a_mean_and_two_bands() {
    for (level, step) in SCALES {
        for nan_at in [None, Some(1500)] {
            let d = data(level, step, 3000, nan_at);
            for n in [2usize, 3, 5, 20, 64, 200] {
                for pct in [2.5, 0.1, 20.0] {
                    let got = series::envelopes(&d.c, n as f64, pct).unwrap();
                    let want = envelopes_ref(&d.c, n, pct);
                    for (k, name) in ["upper", "mid", "lower"].iter().enumerate() {
                        max_rel_error(
                            &format!("envelopes({n},{pct}) {name} level {level} nan {nan_at:?}"),
                            &got[k],
                            &want[k],
                            1e-9,
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn midpoint_and_midprice_are_the_same_bits_as_the_window_fold() {
    for (level, step) in SCALES {
        for nan_at in [None, Some(1500)] {
            let d = data(level, step, 3000, nan_at);
            for n in WINDOWS {
                let got = series::midprice(&d.h, &d.l, n as f64).unwrap();
                assert!(
                    same(&got, &midprice_ref(&d.h, &d.l, n)),
                    "midprice({n}) level {level} nan {nan_at:?}"
                );
                let got = series::midpoint(&d.c, n as f64).unwrap();
                assert!(
                    same(&got, &midprice_ref(&d.c, &d.c, n)),
                    "midpoint({n}) level {level} nan {nan_at:?}"
                );
            }
        }
    }
}

#[test]
fn vwma_is_the_volume_weighted_mean_of_each_window() {
    for (level, step) in SCALES {
        for nan_at in [None, Some(1500)] {
            let d = data(level, step, 3000, nan_at);
            for n in WINDOWS {
                max_rel_error(
                    &format!("vwma({n}) level {level} nan {nan_at:?}"),
                    &series::vwma(&d.c, &d.v, n as f64).unwrap(),
                    &vwma_ref(&d.c, &d.v, n),
                    1e-9,
                );
            }
        }
    }
}

#[test]
fn evwma_steps_toward_each_close_by_its_share_of_the_volume() {
    for (level, step) in SCALES {
        let d = data(level, step, 3000, None);
        for n in WINDOWS {
            max_rel_error(
                &format!("evwma({n}) level {level}"),
                &series::evwma(&d.c, &d.v, n as f64).unwrap(),
                &evwma_ref(&d.c, &d.v, n),
                1e-9,
            );
        }
        // A NaN close poisons it for good, in the reference as in the core.
        let d = data(level, step, 3000, Some(1500));
        let got = series::evwma(&d.c, &d.v, 20.0).unwrap();
        max_rel_error(&format!("evwma NaN level {level}"), &got, &evwma_ref(&d.c, &d.v, 20), 1e-9);
        assert!(got[2999].is_nan(), "a NaN close poisons evwma for good");
    }
}

#[test]
fn vidya_matches_the_cmo_form() {
    for (level, step) in SCALES {
        let d = data(level, step, 3000, None);
        for (p, cmo) in [(14usize, 9usize), (2, 2), (20, 5), (64, 64), (200, 200), (3, 150)] {
            max_rel_error(
                &format!("vidya({p},{cmo}) level {level}"),
                &series::vidya(&d.c, p as f64, cmo as f64).unwrap(),
                &vidya_ref(&d.c, p, cmo),
                1e-9,
            );
        }
        let d = data(level, step, 3000, Some(1500));
        let got = series::vidya(&d.c, 14.0, 9.0).unwrap();
        max_rel_error(&format!("vidya NaN level {level}"), &got, &vidya_ref(&d.c, 14, 9), 1e-9);
        assert!(got[2999].is_nan(), "a NaN close poisons vidya for good");
    }
}

#[test]
fn frama_matches_the_fractal_dimension_form() {
    for (level, step) in SCALES {
        let d = data(level, step, 3000, None);
        for n in [2usize, 3, 4, 6, 16, 20, 64, 200, 400] {
            max_rel_error(
                &format!("frama({n}) level {level}"),
                &series::frama(&d.h, &d.l, &d.c, n as f64).unwrap(),
                &frama_ref(&d.h, &d.l, &d.c, n),
                1e-9,
            );
        }
        let d = data(level, step, 3000, Some(1500));
        let got = series::frama(&d.h, &d.l, &d.c, 16.0).unwrap();
        max_rel_error(
            &format!("frama NaN level {level}"),
            &got,
            &frama_ref(&d.h, &d.l, &d.c, 16),
            1e-9,
        );
        assert!(got[2999].is_nan(), "a NaN close poisons frama for good");
    }
}

#[test]
fn hilo_activator_follows_the_trend_rule() {
    for (level, step) in SCALES {
        for nan_at in [None, Some(1500)] {
            let d = data(level, step, 3000, nan_at);
            for n in [1usize, 2, 3, 5, 20, 64, 200] {
                max_rel_error(
                    &format!("hilo({n}) level {level} nan {nan_at:?}"),
                    &series::hilo_activator(&d.h, &d.l, &d.c, n as f64).unwrap(),
                    &hilo_ref(&d.h, &d.l, &d.c, n),
                    1e-9,
                );
            }
        }
    }
}

#[test]
fn atr_trailing_stop_ratchets_with_the_trend() {
    for (level, step) in SCALES {
        for nan_at in [None, Some(1500)] {
            let d = data(level, step, 3000, nan_at);
            for (n, mult) in [(14usize, 3.0), (1, 0.5), (2, 10.0), (20, 1.5), (64, 3.0), (200, 3.0)]
            {
                max_rel_error(
                    &format!("atr_trailing_stop({n},{mult}) level {level} nan {nan_at:?}"),
                    &series::atr_trailing_stop(&d.h, &d.l, &d.c, n as f64, mult).unwrap(),
                    &atr_stop_ref(&d.h, &d.l, &d.c, n, mult),
                    1e-9,
                );
            }
        }
    }
}

// ------------------------------------------------------------------ the awkward inputs

/// A volume spike that leaves the window must leave nothing of its rounding behind, and a window
/// with no volume is `NaN` for the weighted mean and carries the previous value for the elastic one.
#[test]
fn volume_spikes_and_empty_windows_leave_no_residue() {
    let c = closes(100.0, 2.0, 1200);
    let mut v: Vec<f64> = (0..1200).map(volume_at).collect();
    v[300] = 3.0e9;
    v[301] = 1.0e12;
    for x in &mut v[700..790] {
        *x = 0.0;
    }
    for n in [2usize, 5, 20, 64, 200] {
        let got = series::vwma(&c, &v, n as f64).unwrap();
        max_rel_error(&format!("vwma spike n={n}"), &got, &vwma_ref(&c, &v, n), 1e-9);
        for (i, g) in got.iter().enumerate().take(790).skip(700 + n) {
            assert!(g.is_nan(), "vwma({n}) at {i}: a window with no volume is NaN, got {g}");
        }
        let got = series::evwma(&c, &v, n as f64).unwrap();
        max_rel_error(&format!("evwma spike n={n}"), &got, &evwma_ref(&c, &v, n), 1e-9);
        for i in 700 + n..790 {
            assert_eq!(
                got[i],
                got[699 + n],
                "evwma({n}) at {i} carries the value across no volume"
            );
        }
    }
}

/// A NaN volume makes the volume-weighted values NaN (vwma) or carried (evwma) while it is inside
/// the window, and both are exact the bar after it left.
#[test]
fn a_nan_volume_leaves_the_window_cleanly() {
    let c = closes(100.0, 2.0, 1200);
    let mut v: Vec<f64> = (0..1200).map(volume_at).collect();
    v[600] = f64::NAN;
    for n in [2usize, 5, 20, 64] {
        max_rel_error(
            &format!("vwma NaN volume n={n}"),
            &series::vwma(&c, &v, n as f64).unwrap(),
            &vwma_ref(&c, &v, n),
            1e-9,
        );
        max_rel_error(
            &format!("evwma NaN volume n={n}"),
            &series::evwma(&c, &v, n as f64).unwrap(),
            &evwma_ref(&c, &v, n),
            1e-9,
        );
    }
}

#[test]
fn vidya_stays_put_once_its_window_is_flat() {
    let mut c = closes(100.0, 5.0, 700);
    c.extend(std::iter::repeat_n(c[699], 100));
    for (p, cmo) in [(14usize, 9usize), (20, 50), (3, 2)] {
        let got = series::vidya(&c, p as f64, cmo as f64).unwrap();
        for i in 700 + cmo..800 {
            assert_eq!(got[i], got[699 + cmo], "vidya({p},{cmo}) at {i} moved over a flat window");
        }
    }
}

#[test]
fn midpoint_of_a_series_is_midprice_of_it_as_high_and_low() {
    let c = closes(100.0, 2.0, 2000);
    for n in WINDOWS {
        assert!(
            same(
                &series::midpoint(&c, n as f64).unwrap(),
                &series::midprice(&c, &c, n as f64).unwrap()
            ),
            "n={n}"
        );
    }
}

// ------------------------------------------------------------------ first values

#[test]
fn the_first_value_is_where_the_registry_says() {
    let d = data(100.0, 2.0, 700, None);
    let first = |v: &[f64]| v.iter().position(|x| !x.is_nan());
    for n in [2usize, 5, 20, 64, 200, 400] {
        let nf = n as f64;
        assert_eq!(
            first(&series::alma(&d.c, nf, 0.85, 6.0).unwrap()),
            Some(Alma::new(nf, 0.85, 6.0).unwrap().lookback())
        );
        assert_eq!(
            first(&series::midpoint(&d.c, nf).unwrap()),
            Some(Midpoint::new(nf).unwrap().lookback())
        );
        assert_eq!(
            first(&series::midprice(&d.h, &d.l, nf).unwrap()),
            Some(Midprice::new(nf).unwrap().lookback())
        );
        assert_eq!(
            first(&series::vwma(&d.c, &d.v, nf).unwrap()),
            Some(Vwma::new(nf).unwrap().lookback())
        );
        assert_eq!(
            first(&series::evwma(&d.c, &d.v, nf).unwrap()),
            Some(Evwma::new(nf).unwrap().lookback())
        );
        assert_eq!(
            first(&series::frama(&d.h, &d.l, &d.c, nf).unwrap()),
            Some(Frama::new(nf).unwrap().lookback())
        );
        if n <= 200 {
            assert_eq!(
                first(&series::envelopes(&d.c, nf, 2.5).unwrap()[1]),
                Some(Envelopes::new(nf, 2.5).unwrap().lookback())
            );
            assert_eq!(
                first(&series::hilo_activator(&d.h, &d.l, &d.c, nf).unwrap()),
                Some(HiloActivator::new(nf).unwrap().lookback())
            );
            assert_eq!(
                first(&series::atr_trailing_stop(&d.h, &d.l, &d.c, nf, 3.0).unwrap()),
                Some(AtrTrailingStop::new(nf, 3.0).unwrap().lookback())
            );
            assert_eq!(
                first(&series::vidya(&d.c, 14.0, nf).unwrap()),
                Some(Vidya::new(14.0, nf).unwrap().lookback())
            );
        }
    }
    for n in [1usize, 3] {
        let nf = n as f64;
        assert_eq!(
            first(&series::hilo_activator(&d.h, &d.l, &d.c, nf).unwrap()),
            Some(HiloActivator::new(nf).unwrap().lookback())
        );
        assert_eq!(
            first(&series::atr_trailing_stop(&d.h, &d.l, &d.c, nf, 3.0).unwrap()),
            Some(AtrTrailingStop::new(nf, 3.0).unwrap().lookback())
        );
    }
}

// ------------------------------------------------------------------ every way in, the same bits

fn lengths(n: usize) -> [usize; 8] {
    [0, 1, n.saturating_sub(1), n, n + 1, 3 * n, 3 * n + 1, 2600]
}

fn bars_with_nan(len: usize) -> Vec<Bar> {
    let mut c = closes(100.0, 2.0, len);
    if len > 40 {
        c[len / 3] = f64::NAN;
    }
    bars_of(&c, 2.0)
}

/// Compares the four ways in of a one-line indicator over `b`: `next(&Bar)` (the stream), the plain
/// number door, `vectorize` and the slice call.
macro_rules! ways {
    ($label:expr, $mk:expr, $door:expr, $series:expr, $b:expr) => {{
        let b: &[Bar] = $b;
        let mut ind = $mk;
        let stream: Vec<f64> = b.iter().map(|x| ind.next(x)).collect();
        let mut ind = $mk;
        let door: Vec<f64> = b.iter().map(|x| $door(&mut ind, x)).collect();
        assert!(same(&stream, &door), "{}: next vs the plain-number door", $label);
        assert!(same(&stream, &$mk.vectorize(b)[0]), "{}: vectorize", $label);
        let slice: Vec<f64> = $series;
        assert!(same(&stream, &slice), "{}: series", $label);
    }};
}

#[test]
fn every_way_in_gives_the_same_bits() {
    for n in [2usize, 3, 7, 20, 64, 200, 400] {
        for len in lengths(n) {
            let b = bars_with_nan(len);
            let (c, h, l, v): (Vec<f64>, Vec<f64>, Vec<f64>, Vec<f64>) =
                (b.iter().map(|x| x.close).collect(), highs(&b), lows(&b), vols(&b));
            let nf = n as f64;
            let tag = |s: &str| format!("{s} n={n} len={len}");

            ways!(
                tag("alma"),
                Alma::new(nf, 0.85, 6.0).unwrap(),
                |i: &mut Alma, x: &Bar| i.next_value(x.close),
                series::alma(&c, nf, 0.85, 6.0).unwrap(),
                &b
            );
            ways!(
                tag("midpoint"),
                Midpoint::new(nf).unwrap(),
                |i: &mut Midpoint, x: &Bar| i.next_value(x.close),
                series::midpoint(&c, nf).unwrap(),
                &b
            );
            ways!(
                tag("midprice"),
                Midprice::new(nf).unwrap(),
                |i: &mut Midprice, x: &Bar| i.next_hl(x.high, x.low),
                series::midprice(&h, &l, nf).unwrap(),
                &b
            );
            ways!(
                tag("vwma"),
                Vwma::new(nf).unwrap(),
                |i: &mut Vwma, x: &Bar| i.next_cv(x.close, x.volume),
                series::vwma(&c, &v, nf).unwrap(),
                &b
            );
            ways!(
                tag("evwma"),
                Evwma::new(nf).unwrap(),
                |i: &mut Evwma, x: &Bar| i.next_cv(x.close, x.volume),
                series::evwma(&c, &v, nf).unwrap(),
                &b
            );
            ways!(
                tag("frama"),
                Frama::new(nf).unwrap(),
                |i: &mut Frama, x: &Bar| i.next_hlc(x.high, x.low, x.close),
                series::frama(&h, &l, &c, nf).unwrap(),
                &b
            );
            if n <= 200 {
                ways!(
                    tag("vidya"),
                    Vidya::new(14.0, nf).unwrap(),
                    |i: &mut Vidya, x: &Bar| i.next_value(x.close),
                    series::vidya(&c, 14.0, nf).unwrap(),
                    &b
                );
                ways!(
                    tag("vidya period"),
                    Vidya::new(nf, 9.0).unwrap(),
                    |i: &mut Vidya, x: &Bar| i.next_value(x.close),
                    series::vidya(&c, nf, 9.0).unwrap(),
                    &b
                );
                ways!(
                    tag("hilo_activator"),
                    HiloActivator::new(nf).unwrap(),
                    |i: &mut HiloActivator, x: &Bar| i.next_hlc(x.high, x.low, x.close),
                    series::hilo_activator(&h, &l, &c, nf).unwrap(),
                    &b
                );
                ways!(
                    tag("atr_trailing_stop"),
                    AtrTrailingStop::new(nf, 3.0).unwrap(),
                    |i: &mut AtrTrailingStop, x: &Bar| i.next_hlc(x.high, x.low, x.close),
                    series::atr_trailing_stop(&h, &l, &c, nf, 3.0).unwrap(),
                    &b
                );

                // Envelopes has three lines.
                let mut ind = Envelopes::new(nf, 2.5).unwrap();
                let stream: Vec<[f64; 3]> = b.iter().map(|x| ind.next(x)).collect();
                let mut ind = Envelopes::new(nf, 2.5).unwrap();
                let door: Vec<[f64; 3]> = c.iter().map(|&x| ind.next_value(x)).collect();
                let vec = Envelopes::new(nf, 2.5).unwrap().vectorize(&b);
                let slice = series::envelopes(&c, nf, 2.5).unwrap();
                for k in 0..3 {
                    let line: Vec<f64> = stream.iter().map(|r| r[k]).collect();
                    let door_line: Vec<f64> = door.iter().map(|r| r[k]).collect();
                    assert!(same(&line, &door_line), "envelopes n={n} len={len} line {k}: door");
                    assert!(same(&line, &vec[k]), "envelopes n={n} len={len} line {k}: vectorize");
                    assert!(same(&line, &slice[k]), "envelopes n={n} len={len} line {k}: series");
                }
            }
        }
    }
}

#[test]
fn reset_returns_to_the_first_bar() {
    let b = bars_of(&closes(100.0, 2.0, 700), 2.0);
    fn run<I: Indicator>(ind: &mut I, b: &[Bar]) -> Vec<f64> {
        b.iter().map(|x| ind.on_bar(x)[0]).collect()
    }
    macro_rules! resets {
        ($($name:literal => $ty:ty),* $(,)?) => {$(
            let mut ind = <$ty>::default();
            let first = run(&mut ind, &b);
            ind.reset();
            assert!(same(&first, &run(&mut ind, &b)), $name);
        )*};
    }
    resets!(
        "alma" => Alma, "midpoint" => Midpoint, "midprice" => Midprice, "vwma" => Vwma,
        "evwma" => Evwma, "frama" => Frama, "vidya" => Vidya, "hilo_activator" => HiloActivator,
        "atr_trailing_stop" => AtrTrailingStop, "envelopes" => Envelopes,
    );
}

/// The numbers the report quotes: the worst relative error against the textbook, per indicator,
/// over every scale, window and NaN position this file uses.
///
/// cargo test -p vike-indicators --release --test fast_e -- --ignored --nocapture
#[test]
#[ignore = "prints the measured worst errors"]
fn report_worst_errors() {
    let mut w = [0.0_f64; 10];
    for (level, step) in SCALES {
        let d = data(level, step, 4000, None);
        for n in WINDOWS {
            let e = |got: &[f64], want: &[f64]| max_rel_error("report", got, want, 1.0);
            w[0] = w[0].max(e(
                &series::alma(&d.c, n as f64, 0.85, 6.0).unwrap(),
                &alma_ref(&d.c, n, 0.85, 6.0),
            ));
            if n <= 200 {
                let got = series::envelopes(&d.c, n as f64, 2.5).unwrap();
                let want = envelopes_ref(&d.c, n, 2.5);
                for k in 0..3 {
                    w[1] = w[1].max(e(&got[k], &want[k]));
                }
            }
            w[2] = w[2]
                .max(e(&series::midpoint(&d.c, n as f64).unwrap(), &midprice_ref(&d.c, &d.c, n)));
            w[3] = w[3].max(e(
                &series::midprice(&d.h, &d.l, n as f64).unwrap(),
                &midprice_ref(&d.h, &d.l, n),
            ));
            w[4] =
                w[4].max(e(&series::vwma(&d.c, &d.v, n as f64).unwrap(), &vwma_ref(&d.c, &d.v, n)));
            w[5] = w[5]
                .max(e(&series::evwma(&d.c, &d.v, n as f64).unwrap(), &evwma_ref(&d.c, &d.v, n)));
            w[6] = w[6].max(e(
                &series::frama(&d.h, &d.l, &d.c, n as f64).unwrap(),
                &frama_ref(&d.h, &d.l, &d.c, n),
            ));
            if n <= 200 {
                w[7] = w[7].max(e(
                    &series::hilo_activator(&d.h, &d.l, &d.c, n as f64).unwrap(),
                    &hilo_ref(&d.h, &d.l, &d.c, n),
                ));
                w[8] = w[8].max(e(
                    &series::atr_trailing_stop(&d.h, &d.l, &d.c, n as f64, 3.0).unwrap(),
                    &atr_stop_ref(&d.h, &d.l, &d.c, n, 3.0),
                ));
                w[9] = w[9]
                    .max(e(&series::vidya(&d.c, 14.0, n as f64).unwrap(), &vidya_ref(&d.c, 14, n)));
            }
        }
    }
    println!(
        "worst relative error: alma {:e}, envelopes {:e}, midpoint {:e}, midprice {:e}, vwma {:e}, \
         evwma {:e}, frama {:e}, hilo_activator {:e}, atr_trailing_stop {:e}, vidya {:e}",
        w[0], w[1], w[2], w[3], w[4], w[5], w[6], w[7], w[8], w[9]
    );
}
