//! `adosc`, `anchored_vwap`, `efi`, `eom`, `kvo`, `net_volume`, `nvi`, `pvi`, `pvt`, `volume_osc`,
//! `williams_ad` and `vwap`: the bounded-state steppers against a textbook reference, and the stream
//! (`next(&Bar)`), the batch (`vectorize`), the plain-number entry (`next_cv`, `next_hlc`,
//! `next_hlcv`, `next_hlv`, `next_value`, `next_ts_hlcv`) and the slice call (`series::*`) against
//! each other, bit for bit.
//!
//! The reference does each step the long way, in the expressions of the kernels the steppers replaced:
//! the averages are the kernels' `ema` / `sma` over the whole column (the `NaN` entries compacted away
//! and scattered back where the kernel did that), the cumulative series are plain left folds. The
//! steppers run the same arithmetic in the same order (`eom`'s mean is a ring sum where the kernel
//! re-folded each window; the two VWAPs multiply by `1 / 3` where the kernel divided), so most agree with
//! the reference to the last bit and the tolerance is for those three: 1e-9 relative, on three scales of
//! series.

use vike_indicators::indicators::{
    Adosc, AnchoredVwap, Efi, Eom, Kvo, NetVolume, Nvi, Pvi, Pvt, VolumeOsc, Vwap, WilliamsAd,
};
use vike_indicators::{Indicator, series};
use vike_marketdata::Bar;

use crate::fast::{
    Cols, SPREAD_SCALES, assert_close, closes, cols, ema_ref, same, sma, worst_floor_one,
};

/// A bar around each close with a spread that varies, a volume that rises and falls, and a stretch
/// of bars with no volume. One bar a minute, so a UTC day ends after 1440 bars.
fn bars_of(c: &[f64], scale: f64, ts_step: i64) -> Vec<Bar> {
    c.iter()
        .enumerate()
        .map(|(i, &x)| {
            let up = (0.3 + (i % 7) as f64 * 0.1) * scale;
            let dn = (0.2 + (i % 5) as f64 * 0.15) * scale;
            let vol = if (150..190).contains(&i) { 0.0 } else { 100.0 + (i % 13) as f64 * 7.0 };
            Bar::ohlcv(i as i64 * ts_step, x, x + up, x - dn, x, vol)
        })
        .collect()
}

// ---- the textbook references ----------------------------------------------------------------

/// `math::smooth_defined`: smooth the non-`NaN` entries and scatter the result back.
fn smooth_defined_ref(src: &[f64], ma: fn(&[f64], usize) -> Vec<f64>, n: usize) -> Vec<f64> {
    let defined: Vec<(usize, f64)> =
        src.iter().enumerate().filter(|(_, v)| !v.is_nan()).map(|(i, v)| (i, *v)).collect();
    let mut out = vec![f64::NAN; src.len()];
    if defined.len() >= n {
        let values: Vec<f64> = defined.iter().map(|(_, v)| *v).collect();
        for ((i, _), s) in defined.iter().zip(ma(&values, n)) {
            out[*i] = s;
        }
    }
    out
}

fn adosc_ref(x: &Cols, fast: usize, slow: usize) -> Vec<f64> {
    let mut acc = 0.0;
    let ad: Vec<f64> = (0..x.c.len())
        .map(|i| {
            let rng = x.h[i] - x.l[i];
            let clv = if rng != 0.0 { ((x.c[i] - x.l[i]) - (x.h[i] - x.c[i])) / rng } else { 0.0 };
            let flow = clv * x.v[i];
            acc = if i == 0 { flow } else { acc + flow };
            acc
        })
        .collect();
    let (f, s) = (ema_ref(&ad, fast), ema_ref(&ad, slow));
    f.iter().zip(&s).map(|(a, b)| if a.is_nan() || b.is_nan() { f64::NAN } else { a - b }).collect()
}

fn vwap_ref(x: &Cols) -> Vec<f64> {
    let (mut pv, mut vol, mut day) = (0.0, 0.0, i64::MIN);
    (0..x.c.len())
        .map(|i| {
            let d = x.ts[i] / 86_400_000;
            if d != day {
                pv = 0.0;
                vol = 0.0;
                day = d;
            }
            let tp = (x.h[i] + x.l[i] + x.c[i]) / 3.0;
            pv += tp * x.v[i];
            vol += x.v[i];
            if vol > 0.0 { pv / vol } else { f64::NAN }
        })
        .collect()
}

fn anchored_ref(x: &Cols, anchor: f64) -> Vec<f64> {
    let (mut pv, mut vol, mut started) = (0.0, 0.0, false);
    (0..x.c.len())
        .map(|i| {
            if !started && (x.ts[i] as f64) >= anchor {
                started = true;
            }
            if !started {
                return f64::NAN;
            }
            let tp = (x.h[i] + x.l[i] + x.c[i]) / 3.0;
            pv += tp * x.v[i];
            vol += x.v[i];
            if vol > 0.0 { pv / vol } else { f64::NAN }
        })
        .collect()
}

fn efi_ref(x: &Cols, n: usize) -> Vec<f64> {
    let raw: Vec<f64> = (0..x.c.len())
        .map(|i| if i == 0 { f64::NAN } else { (x.c[i] - x.c[i - 1]) * x.v[i] })
        .collect();
    smooth_defined_ref(&raw, ema_ref, n)
}

fn eom_ref(x: &Cols, n: usize) -> Vec<f64> {
    let raw: Vec<f64> = (0..x.c.len())
        .map(|i| {
            if i == 0 {
                return f64::NAN;
            }
            let mid_move = ((x.h[i] + x.l[i]) / 2.0) - ((x.h[i - 1] + x.l[i - 1]) / 2.0);
            let hl = x.h[i] - x.l[i];
            if hl != 0.0 && x.v[i] != 0.0 { mid_move / (x.v[i] / hl) } else { 0.0 }
        })
        .collect();
    smooth_defined_ref(&raw, sma, n)
}

fn kvo_ref(x: &Cols, nf: usize, ns: usize, nsig: usize) -> [Vec<f64>; 2] {
    let len = x.c.len();
    let hlc3: Vec<f64> = (0..len).map(|i| (x.h[i] + x.l[i] + x.c[i]) / 3.0).collect();
    let mut vf = vec![f64::NAN; len];
    for i in 1..len {
        vf[i] = x.v[i] * if hlc3[i] > hlc3[i - 1] { 1.0 } else { -1.0 };
    }
    let (f, s) = (smooth_defined_ref(&vf, ema_ref, nf), smooth_defined_ref(&vf, ema_ref, ns));
    let kvo: Vec<f64> = f
        .iter()
        .zip(&s)
        .map(|(a, b)| if a.is_nan() || b.is_nan() { f64::NAN } else { a - b })
        .collect();
    let sig = smooth_defined_ref(&kvo, ema_ref, nsig);
    [kvo, sig]
}

fn net_volume_ref(x: &Cols) -> Vec<f64> {
    (0..x.c.len())
        .map(|i| {
            if i == 0 {
                0.0
            } else if x.c[i] > x.c[i - 1] {
                x.v[i]
            } else if x.c[i] < x.c[i - 1] {
                -x.v[i]
            } else {
                0.0
            }
        })
        .collect()
}

fn rocp_ref(c: f64, pc: f64) -> f64 {
    if pc != 0.0 { (c - pc) / pc } else { 0.0 }
}

/// `negative`: the index moves on a volume fall; else on a rise.
fn volume_index_ref(x: &Cols, negative: bool) -> Vec<f64> {
    let mut index = 1000.0;
    (0..x.c.len())
        .map(|i| {
            if i > 0 {
                let moves = if negative { x.v[i] < x.v[i - 1] } else { x.v[i] > x.v[i - 1] };
                if moves {
                    index *= 1.0 + rocp_ref(x.c[i], x.c[i - 1]);
                }
            }
            index
        })
        .collect()
}

fn pvt_ref(x: &Cols) -> Vec<f64> {
    let mut acc = 0.0;
    (0..x.c.len())
        .map(|i| {
            if i > 0 {
                acc += x.v[i] * rocp_ref(x.c[i], x.c[i - 1]);
            }
            acc
        })
        .collect()
}

fn williams_ad_ref(x: &Cols) -> Vec<f64> {
    let mut acc = 0.0;
    (0..x.c.len())
        .map(|i| {
            if i > 0 {
                let (h, l, c, pc) = (x.h[i], x.l[i], x.c[i], x.c[i - 1]);
                acc += if c > pc {
                    c - l.min(pc)
                } else if c < pc {
                    c - h.max(pc)
                } else {
                    0.0
                };
            }
            acc
        })
        .collect()
}

fn volume_osc_ref(x: &Cols, short: usize, long: usize) -> Vec<f64> {
    let (s, l) = (ema_ref(&x.v, short), ema_ref(&x.v, long));
    (0..x.v.len())
        .map(|i| {
            if !s[i].is_nan() && !l[i].is_nan() && l[i] != 0.0 {
                (s[i] - l[i]) / l[i] * 100.0
            } else {
                f64::NAN
            }
        })
        .collect()
}

// ---- comparisons ------------------------------------------------------------------------------

/// Each scale clean, with a `NaN` price planted in the middle, and with a `NaN` volume planted.
fn inputs() -> Vec<(String, Cols)> {
    let mut out = Vec::new();
    for (level, step, spread) in SPREAD_SCALES {
        for plant in ["clean", "nan price", "nan volume", "early nan price", "early nan volume"] {
            if plant.starts_with("early") && level != 100.0 {
                continue;
            }
            let mut c = closes(level, step, 3000);
            if plant == "nan price" {
                c[1200] = f64::NAN;
            }
            if plant == "early nan price" {
                c[30] = f64::NAN; // inside the seed of every average longer than 30
            }
            let mut b = bars_of(&c, spread, 60_000);
            if plant == "nan volume" {
                b[1300].volume = f64::NAN;
            }
            if plant == "early nan volume" {
                b[45].volume = f64::NAN;
            }
            out.push((format!("level {level} {plant}"), cols(&b)));
        }
    }
    out
}

/// Hourly bars over 120 days: a UTC day ends every 24 bars.
fn hourly() -> Cols {
    cols(&bars_of(&closes(100.0, 2.0, 2880), 1.0, 3_600_000))
}

#[test]
fn adosc_matches_the_kernel() {
    for (what, x) in inputs() {
        for (f, s) in [(2usize, 2usize), (3, 10), (5, 3), (20, 64), (50, 200), (7, 13)] {
            assert_close(
                &format!("adosc({f},{s}) {what}"),
                &series::adosc(&x.h, &x.l, &x.c, &x.v, f as f64, s as f64).unwrap(),
                &adosc_ref(&x, f, s),
                1e-9,
                1.0,
            );
        }
    }
}

#[test]
fn vwap_matches_the_session_fold_and_restarts_each_day() {
    for (what, x) in inputs().into_iter().chain([("hourly".to_string(), hourly())]) {
        let got = series::vwap(&x.ts, &x.h, &x.l, &x.c, &x.v);
        assert_close(&format!("vwap {what}"), &got, &vwap_ref(&x), 1e-9, 1.0);
    }
    // The first bar of a day is that bar's own typical price: nothing carries over midnight.
    let x = hourly();
    let got = series::vwap(&x.ts, &x.h, &x.l, &x.c, &x.v);
    for i in (24..x.c.len()).step_by(24) {
        if x.v[i] == 0.0 {
            continue; // a day that opens with no volume has no VWAP yet
        }
        let tp = (x.h[i] + x.l[i] + x.c[i]) / 3.0;
        assert!((got[i] - tp).abs() <= 1e-9 * tp.abs(), "vwap[{i}] restarts at midnight");
    }
}

#[test]
fn anchored_vwap_matches_the_fold_from_the_anchor() {
    for (what, x) in inputs() {
        for anchor in [0.0, 700.0 * 60_000.0, 2_999.0 * 60_000.0, 4.0e12] {
            let got = series::anchored_vwap(&x.ts, &x.h, &x.l, &x.c, &x.v, anchor).unwrap();
            assert_close(
                &format!("anchored_vwap({anchor}) {what}"),
                &got,
                &anchored_ref(&x, anchor),
                1e-9,
                1.0,
            );
        }
    }
    // Before the anchor there is nothing.
    let x = inputs().remove(0).1;
    let got = series::anchored_vwap(&x.ts, &x.h, &x.l, &x.c, &x.v, 700.0 * 60_000.0).unwrap();
    assert!(got[..700].iter().all(|v| v.is_nan()) && got[700].is_finite());
}

#[test]
fn efi_matches_the_kernel() {
    for (what, x) in inputs() {
        for n in [2usize, 3, 13, 64, 200] {
            assert_close(
                &format!("efi({n}) {what}"),
                &series::efi(&x.c, &x.v, n as f64).unwrap(),
                &efi_ref(&x, n),
                1e-9,
                1.0,
            );
        }
    }
}

#[test]
fn eom_matches_the_window_fold() {
    for (what, x) in inputs() {
        for n in [2usize, 3, 14, 64, 200] {
            assert_close(
                &format!("eom({n}) {what}"),
                &series::eom(&x.h, &x.l, &x.v, n as f64).unwrap(),
                &eom_ref(&x, n),
                1e-9,
                1.0,
            );
        }
    }
}

#[test]
fn kvo_matches_the_kernel() {
    for (what, x) in inputs() {
        for (f, s, g) in
            [(2usize, 2usize, 2usize), (34, 55, 13), (5, 3, 4), (100, 500, 100), (200, 2, 50)]
        {
            let got = series::kvo(&x.h, &x.l, &x.c, &x.v, f as f64, s as f64, g as f64).unwrap();
            let want = kvo_ref(&x, f, s, g);
            for line in 0..2 {
                assert_close(
                    &format!("kvo({f},{s},{g}) line {line} {what}"),
                    &got[line],
                    &want[line],
                    1e-9,
                    1.0,
                );
            }
        }
    }
}

#[test]
fn the_cumulative_series_match_the_kernels() {
    for (what, x) in inputs() {
        assert_close(
            &format!("net_volume {what}"),
            &series::net_volume(&x.c, &x.v),
            &net_volume_ref(&x),
            1e-9,
            1.0,
        );
        assert_close(
            &format!("nvi {what}"),
            &series::nvi(&x.c, &x.v),
            &volume_index_ref(&x, true),
            1e-9,
            1.0,
        );
        assert_close(
            &format!("pvi {what}"),
            &series::pvi(&x.c, &x.v),
            &volume_index_ref(&x, false),
            1e-9,
            1.0,
        );
        assert_close(&format!("pvt {what}"), &series::pvt(&x.c, &x.v), &pvt_ref(&x), 1e-9, 1.0);
        assert_close(
            &format!("williams_ad {what}"),
            &series::williams_ad(&x.h, &x.l, &x.c),
            &williams_ad_ref(&x),
            1e-9,
            1.0,
        );
    }
}

#[test]
fn volume_osc_matches_the_kernel() {
    for (what, x) in inputs() {
        for (s, l) in [(2usize, 2usize), (5, 10), (10, 5), (50, 200), (2, 200)] {
            assert_close(
                &format!("volume_osc({s},{l}) {what}"),
                &series::volume_osc(&x.v, s as f64, l as f64).unwrap(),
                &volume_osc_ref(&x, s, l),
                1e-9,
                1.0,
            );
        }
    }
}

/// A volume that never moves has no net volume, no index move and a zero oscillator, exactly.
#[test]
fn a_flat_volume_is_exactly_flat() {
    let b: Vec<Bar> = closes(100.0, 2.0, 400)
        .iter()
        .enumerate()
        .map(|(i, &x)| Bar::ohlcv(i as i64 * 60_000, x, x + 1.0, x - 1.0, x, 250.0))
        .collect();
    let x = cols(&b);
    assert!(series::nvi(&x.c, &x.v).iter().all(|&v| v == 1000.0));
    assert!(series::pvi(&x.c, &x.v).iter().all(|&v| v == 1000.0));
    let osc = series::volume_osc(&x.v, 5.0, 10.0).unwrap();
    assert!(osc[9..].iter().all(|&v| v.abs() < 1e-9), "a constant volume has no oscillation");
}

/// Stream == next_* == vectorize == series, bit for bit, over parameters, lengths and a `NaN`.
#[test]
fn every_way_in_gives_the_same_bits() {
    let lengths = |n: usize| [0usize, 1, n.saturating_sub(1), n, n + 1, 3 * n, 3 * n + 1, 2600];
    // A `NaN` price a third of the way in, and a `NaN` volume two thirds of the way in.
    let planted = |len: usize, hourly: bool| {
        let mut c = closes(100.0, 2.0, len);
        if len > 40 {
            c[len / 3] = f64::NAN;
        }
        let mut b = bars_of(&c, 1.0, if hourly { 3_600_000 } else { 60_000 });
        if len > 40 {
            b[2 * len / 3].volume = f64::NAN;
        }
        b
    };
    macro_rules! scalar {
        ($name:literal, $make:expr, $len:expr, $plain:expr, $series:expr) => {{
            for hourly in [false, true] {
                let b = planted($len, hourly);
                let x = cols(&b);
                let mut ind = $make;
                let stream: Vec<f64> = b.iter().map(|y| ind.next(y)).collect();
                let mut ind = $make;
                let plain: Vec<f64> = b.iter().map(|y| ($plain)(&mut ind, y)).collect();
                assert!(same(&stream, &plain), "{} len={}: next vs the plain entry", $name, $len);
                assert!(
                    same(&stream, &$make.vectorize(&b)[0]),
                    "{} len={}: vectorize",
                    $name,
                    $len
                );
                assert!(same(&stream, &($series)(&x)), "{} len={}: series", $name, $len);
            }
        }};
    }
    for (f, s) in [(2usize, 2usize), (3, 10), (5, 3), (20, 64), (50, 200)] {
        for len in lengths(f.max(s)) {
            let (ff, sf) = (f as f64, s as f64);
            scalar!(
                "adosc",
                Adosc::new(ff, sf).unwrap(),
                len,
                |i: &mut Adosc, y: &Bar| i.next_hlcv(y.high, y.low, y.close, y.volume),
                |x: &Cols| series::adosc(&x.h, &x.l, &x.c, &x.v, ff, sf).unwrap()
            );
        }
    }
    for anchor in [0.0, 700.0 * 60_000.0, 40.0 * 3_600_000.0, 4.0e12] {
        for len in [0usize, 1, 2, 41, 700, 2600] {
            scalar!(
                "anchored_vwap",
                AnchoredVwap::new(anchor).unwrap(),
                len,
                |i: &mut AnchoredVwap, y: &Bar| i
                    .next_ts_hlcv(y.ts, y.high, y.low, y.close, y.volume),
                |x: &Cols| series::anchored_vwap(&x.ts, &x.h, &x.l, &x.c, &x.v, anchor).unwrap()
            );
        }
    }
    for len in [0usize, 1, 2, 41, 700, 2600] {
        scalar!(
            "vwap",
            Vwap::default(),
            len,
            |i: &mut Vwap, y: &Bar| i.next_ts_hlcv(y.ts, y.high, y.low, y.close, y.volume),
            |x: &Cols| series::vwap(&x.ts, &x.h, &x.l, &x.c, &x.v)
        );
        scalar!(
            "net_volume",
            NetVolume::default(),
            len,
            |i: &mut NetVolume, y: &Bar| i.next_cv(y.close, y.volume),
            |x: &Cols| series::net_volume(&x.c, &x.v)
        );
        scalar!(
            "nvi",
            Nvi::default(),
            len,
            |i: &mut Nvi, y: &Bar| i.next_cv(y.close, y.volume),
            |x: &Cols| series::nvi(&x.c, &x.v)
        );
        scalar!(
            "pvi",
            Pvi::default(),
            len,
            |i: &mut Pvi, y: &Bar| i.next_cv(y.close, y.volume),
            |x: &Cols| series::pvi(&x.c, &x.v)
        );
        scalar!(
            "pvt",
            Pvt::default(),
            len,
            |i: &mut Pvt, y: &Bar| i.next_cv(y.close, y.volume),
            |x: &Cols| series::pvt(&x.c, &x.v)
        );
        scalar!(
            "williams_ad",
            WilliamsAd::default(),
            len,
            |i: &mut WilliamsAd, y: &Bar| i.next_hlc(y.high, y.low, y.close),
            |x: &Cols| series::williams_ad(&x.h, &x.l, &x.c)
        );
    }
    for n in [2usize, 3, 13, 64, 200] {
        for len in lengths(n) {
            let nf = n as f64;
            scalar!(
                "efi",
                Efi::new(nf).unwrap(),
                len,
                |i: &mut Efi, y: &Bar| i.next_cv(y.close, y.volume),
                |x: &Cols| series::efi(&x.c, &x.v, nf).unwrap()
            );
            scalar!(
                "eom",
                Eom::new(nf).unwrap(),
                len,
                |i: &mut Eom, y: &Bar| i.next_hlv(y.high, y.low, y.volume),
                |x: &Cols| series::eom(&x.h, &x.l, &x.v, nf).unwrap()
            );
        }
    }
    for (s, l) in [(2usize, 2usize), (5, 10), (10, 5), (50, 200), (2, 200)] {
        for len in lengths(s.max(l)) {
            let (sf, lf) = (s as f64, l as f64);
            scalar!(
                "volume_osc",
                VolumeOsc::new(sf, lf).unwrap(),
                len,
                |i: &mut VolumeOsc, y: &Bar| i.next_value(y.volume),
                |x: &Cols| series::volume_osc(&x.v, sf, lf).unwrap()
            );
        }
    }
    // kvo has two lines.
    for (f, s, g) in
        [(2usize, 2usize, 2usize), (34, 55, 13), (5, 3, 4), (100, 500, 100), (200, 2, 50)]
    {
        for len in lengths(f.max(s) + g) {
            for hourly in [false, true] {
                let b = planted(len, hourly);
                let x = cols(&b);
                let (ff, sf, gf) = (f as f64, s as f64, g as f64);
                let mut ind = Kvo::new(ff, sf, gf).unwrap();
                let stream: Vec<[f64; 2]> = b.iter().map(|y| ind.next(y)).collect();
                let mut ind = Kvo::new(ff, sf, gf).unwrap();
                let plain: Vec<[f64; 2]> =
                    b.iter().map(|y| ind.next_hlcv(y.high, y.low, y.close, y.volume)).collect();
                let batch = Kvo::new(ff, sf, gf).unwrap().vectorize(&b);
                let sl = series::kvo(&x.h, &x.l, &x.c, &x.v, ff, sf, gf).unwrap();
                for line in 0..2 {
                    let st: Vec<f64> = stream.iter().map(|v| v[line]).collect();
                    let pl: Vec<f64> = plain.iter().map(|v| v[line]).collect();
                    assert!(same(&st, &pl), "kvo ({f},{s},{g}) len={len} line {line}: next_hlcv");
                    assert!(
                        same(&st, &batch[line]),
                        "kvo ({f},{s},{g}) len={len} line {line}: vectorize"
                    );
                    assert!(
                        same(&st, &sl[line]),
                        "kvo ({f},{s},{g}) len={len} line {line}: series"
                    );
                }
            }
        }
    }
}

/// `reset` returns a stepper to its first state, and a clone carries on identically.
#[test]
fn reset_and_clone_restore_the_state() {
    let b = bars_of(&closes(100.0, 2.0, 1700), 1.0, 60_000);
    crate::fast::assert_reset_and_fork_agree!(b, 2_000_000_000, Adosc::new(3.0, 10.0).unwrap());
    crate::fast::assert_reset_and_fork_agree!(
        b,
        2_000_000_000,
        AnchoredVwap::new(600.0 * 60_000.0).unwrap()
    );
    crate::fast::assert_reset_and_fork_agree!(b, 2_000_000_000, Efi::new(13.0).unwrap());
    crate::fast::assert_reset_and_fork_agree!(b, 2_000_000_000, Eom::new(14.0).unwrap());
    crate::fast::assert_reset_and_fork_agree!(
        b,
        2_000_000_000,
        Kvo::new(34.0, 55.0, 13.0).unwrap()
    );
    crate::fast::assert_reset_and_fork_agree!(b, 2_000_000_000, NetVolume::default());
    crate::fast::assert_reset_and_fork_agree!(b, 2_000_000_000, Nvi::default());
    crate::fast::assert_reset_and_fork_agree!(b, 2_000_000_000, Pvi::default());
    crate::fast::assert_reset_and_fork_agree!(b, 2_000_000_000, Pvt::default());
    crate::fast::assert_reset_and_fork_agree!(b, 2_000_000_000, VolumeOsc::new(5.0, 10.0).unwrap());
    crate::fast::assert_reset_and_fork_agree!(b, 2_000_000_000, WilliamsAd::default());
    crate::fast::assert_reset_and_fork_agree!(b, 2_000_000_000, Vwap::default());
}

#[test]
fn out_of_range_parameters_are_refused() {
    let e: &[f64] = &[1.0];
    assert!(Adosc::new(1.0, 10.0).is_err() && series::adosc(e, e, e, e, 3.0, 201.0).is_err());
    assert!(
        AnchoredVwap::new(-1.0).is_err()
            && series::anchored_vwap(&[1], e, e, e, e, f64::NAN).is_err()
    );
    assert!(Efi::new(1.0).is_err() && series::efi(e, e, 201.0).is_err());
    assert!(Eom::new(1.0).is_err() && series::eom(e, e, e, 201.0).is_err());
    assert!(
        Kvo::new(34.0, 55.0, 1.0).is_err() && series::kvo(e, e, e, e, 34.0, 501.0, 13.0).is_err()
    );
    assert!(VolumeOsc::new(51.0, 10.0).is_err() && series::volume_osc(e, 5.0, 1.0).is_err());
}

/// The worst relative error of each indicator against its reference over the parameters and scales
/// above, printed (`--ignored --nocapture`); the tests above hold it under 1e-9.
#[test]
#[ignore = "a measurement, not a check"]
fn measure_the_worst_relative_error() {
    let mut w = [0.0_f64; 12];
    for (_, x) in inputs() {
        for (f, s) in [(2usize, 2usize), (3, 10), (5, 3), (20, 64), (50, 200), (7, 13)] {
            let g = series::adosc(&x.h, &x.l, &x.c, &x.v, f as f64, s as f64).unwrap();
            w[0] = w[0].max(worst_floor_one(&g, &adosc_ref(&x, f, s)));
        }
        for anchor in [0.0, 700.0 * 60_000.0] {
            let g = series::anchored_vwap(&x.ts, &x.h, &x.l, &x.c, &x.v, anchor).unwrap();
            w[1] = w[1].max(worst_floor_one(&g, &anchored_ref(&x, anchor)));
        }
        for n in [2usize, 3, 13, 64, 200] {
            let g = series::efi(&x.c, &x.v, n as f64).unwrap();
            w[2] = w[2].max(worst_floor_one(&g, &efi_ref(&x, n)));
            let g = series::eom(&x.h, &x.l, &x.v, n as f64).unwrap();
            w[3] = w[3].max(worst_floor_one(&g, &eom_ref(&x, n)));
        }
        for (f, s, g) in [(2usize, 2usize, 2usize), (34, 55, 13), (5, 3, 4), (100, 500, 100)] {
            let got = series::kvo(&x.h, &x.l, &x.c, &x.v, f as f64, s as f64, g as f64).unwrap();
            let want = kvo_ref(&x, f, s, g);
            w[4] = w[4]
                .max(worst_floor_one(&got[0], &want[0]))
                .max(worst_floor_one(&got[1], &want[1]));
        }
        w[5] = w[5].max(worst_floor_one(&series::net_volume(&x.c, &x.v), &net_volume_ref(&x)));
        w[6] = w[6].max(worst_floor_one(&series::nvi(&x.c, &x.v), &volume_index_ref(&x, true)));
        w[7] = w[7].max(worst_floor_one(&series::pvi(&x.c, &x.v), &volume_index_ref(&x, false)));
        w[8] = w[8].max(worst_floor_one(&series::pvt(&x.c, &x.v), &pvt_ref(&x)));
        for (s, l) in [(2usize, 2usize), (5, 10), (10, 5), (50, 200), (2, 200)] {
            let g = series::volume_osc(&x.v, s as f64, l as f64).unwrap();
            w[9] = w[9].max(worst_floor_one(&g, &volume_osc_ref(&x, s, l)));
        }
        w[10] = w[10]
            .max(worst_floor_one(&series::williams_ad(&x.h, &x.l, &x.c), &williams_ad_ref(&x)));
        w[11] =
            w[11].max(worst_floor_one(&series::vwap(&x.ts, &x.h, &x.l, &x.c, &x.v), &vwap_ref(&x)));
    }
    println!(
        "worst relative error: adosc {:.2e} anchored_vwap {:.2e} efi {:.2e} eom {:.2e} kvo {:.2e} \
         net_volume {:.2e} nvi {:.2e} pvi {:.2e} pvt {:.2e} volume_osc {:.2e} williams_ad {:.2e} \
         vwap {:.2e}",
        w[0], w[1], w[2], w[3], w[4], w[5], w[6], w[7], w[8], w[9], w[10], w[11]
    );
}

/// Zero, negative, infinite and `NaN` closes and volumes: the branch-free cumulative cores must give
/// the textbook answer bit for bit, including the numbers that are not numbers.
#[test]
fn the_cumulative_series_survive_zero_infinite_and_nan_prices() {
    let c = [
        100.0,
        101.0,
        0.0,
        5.0,
        f64::INFINITY,
        7.0,
        f64::NAN,
        8.0,
        -3.0,
        0.0,
        0.0,
        2.0,
        f64::NEG_INFINITY,
        4.0,
        4.0,
        9.0,
    ];
    let v = [
        50.0,
        10.0,
        20.0,
        5.0,
        30.0,
        f64::INFINITY,
        40.0,
        0.0,
        f64::NAN,
        15.0,
        15.0,
        60.0,
        1.0,
        2.0,
        3.0,
        f64::INFINITY,
    ];
    let b: Vec<Bar> = c
        .iter()
        .zip(&v)
        .enumerate()
        .map(|(i, (&c, &v))| Bar::ohlcv(i as i64 * 60_000, c, c + 1.0, c - 1.0, c, v))
        .collect();
    let x = cols(&b);
    assert!(same(&series::net_volume(&x.c, &x.v), &net_volume_ref(&x)), "net_volume");
    assert!(same(&series::nvi(&x.c, &x.v), &volume_index_ref(&x, true)), "nvi");
    assert!(same(&series::pvi(&x.c, &x.v), &volume_index_ref(&x, false)), "pvi");
    assert!(same(&series::pvt(&x.c, &x.v), &pvt_ref(&x)), "pvt");
    assert!(same(&series::williams_ad(&x.h, &x.l, &x.c), &williams_ad_ref(&x)), "williams_ad");
}
