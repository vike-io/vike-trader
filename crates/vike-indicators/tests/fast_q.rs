//! The eighteen two-bar candlestick patterns: the new bounded-state steppers against a COPY of the
//! old kernels (`tests/common/old_two_bar.rs`, the batch kernels the patterns streamed through until
//! the fourth wave), and the ways in against each other.
//!
//! A pattern's signal is discrete (`+100.0`, `-100.0`, `0.0`), so there is no tolerance anywhere
//! here: the stream (`next(&Bar)`), the plain-number entry (`next_ohlc`), `on_bar`, `vectorize`, the
//! slice call (`series::<pattern>`) and the registry's by-name instance must each equal the old
//! kernel's output bit for bit, on three price scales (level 100 with a step of 2, level 1e6 with a
//! step of 0.01, level 1e-3 with a step of 1e-6), at every length around the ten-bar average-body
//! window, and with a `NaN`, an infinity or a negative zero travelling through each of the four
//! prices.
//!
//! The series are built to make every pattern fire (gaps, ties, dojis, marubozu, closes at the prior
//! low or close): a test that never sees a signal proves nothing, so each case also requires its
//! signals to occur, and counts them.

use vike_indicators::indicators::{
    Counterattack, DarkCloudCover, DojiStar, Engulfing, GapSideSideWhite, Harami, HaramiCross,
    HomingPigeon, InNeck, Kicking, KickingByLength, MatchingLow, MeetingLines, OnNeck, Piercing,
    SeparatingLines, TasukiGap, Thrusting,
};
use vike_indicators::{Indicator, series};
use vike_marketdata::Bar;

#[path = "common/old_two_bar.rs"]
mod old;

struct Lcg(u64);

impl Lcg {
    fn unit(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Bars in which every pattern happens often: opens at the prior close, open, low or high, in the
/// prior body, gapped; closes at the open (a doji), the prior close, the prior low, just above the
/// prior close, around the prior midpoint; shadows or none (a marubozu).
fn rich(level: f64, step: f64, n: usize, seed: u64) -> Vec<Bar> {
    let mut r = Lcg(seed);
    let (mut po, mut ph, mut pl, mut pc) = (level, level + step, level - step, level);
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let pb = (pc - po).abs();
        let mid = (po + pc) / 2.0;
        let (lo_b, hi_b) = (po.min(pc), po.max(pc));
        let (m1, m2, m3) = (r.unit(), r.unit(), r.unit());
        let (u1, u2, u3, u4) = (r.unit(), r.unit(), r.unit(), r.unit());
        let open = if m1 < 0.20 {
            pc
        } else if m1 < 0.28 {
            po
        } else if m1 < 0.38 {
            pc + (0.5 + u1) * step
        } else if m1 < 0.48 {
            pc - (0.5 + u1) * step
        } else if m1 < 0.58 {
            lo_b + u1 * (hi_b - lo_b)
        } else if m1 < 0.66 {
            pl - u1 * step
        } else if m1 < 0.74 {
            ph + u1 * step
        } else {
            pc + (u1 - 0.5) * 3.0 * step + (level - pc) * 0.05
        };
        let close = if m2 < 0.10 {
            open + (u2 - 0.5) * 0.01 * step
        } else if m2 < 0.20 {
            pc
        } else if m2 < 0.28 {
            pl
        } else if m2 < 0.36 {
            pc + 0.05 * pb
        } else if m2 < 0.46 {
            mid + (u2 - 0.4) * pb * 0.7
        } else if m2 < 0.54 {
            open + (u2 - 0.5) * 6.0 * step
        } else {
            open + (u2 - 0.5) * 2.0 * step
        };
        let (high, low) = if m3 < 0.25 {
            (open.max(close), open.min(close))
        } else {
            (open.max(close) + u3 * 0.7 * step, open.min(close) - u4 * 0.7 * step)
        };
        out.push(Bar::ohlcv(i as i64 * 60_000, open, high, low, close, 1.0));
        (po, ph, pl, pc) = (open, high, low, close);
    }
    out
}

/// The three scales of the brief: (level, step).
const SCALES: [(f64, f64); 3] = [(100.0, 2.0), (1e6, 0.01), (1e-3, 1e-6)];

/// Lengths around the ten-bar window and a long one.
const LENGTHS: [usize; 14] = [0, 1, 2, 3, 9, 10, 11, 12, 19, 20, 21, 100, 513, 2600];

/// A whole-series call: parallel `open`, `high`, `low` and `close` slices in, one signal per bar out.
type Columns4 = fn(&[f64], &[f64], &[f64], &[f64]) -> Vec<f64>;

struct Case {
    name: &'static str,
    /// The old batch kernel.
    old: fn(&[Bar]) -> Vec<Vec<f64>>,
    series: Columns4,
    stream: fn(&[Bar]) -> Vec<f64>,
    plain: fn(&[Bar]) -> Vec<f64>,
    on_bar: fn(&[Bar]) -> Vec<f64>,
    vectorize: fn(&[Bar]) -> Vec<f64>,
    /// Whether the pattern can signal `+100` / `-100` at all (and so must, on the rich series).
    bull: bool,
    bear: bool,
}

macro_rules! case {
    ($Ty:ident, $name:literal, $old:ident, $series:ident, bull = $bull:literal, bear = $bear:literal) => {
        Case {
            name: $name,
            old: old::$old,
            series: series::$series,
            stream: |bars| {
                let mut p = $Ty::default();
                bars.iter().map(|b| p.next(b)).collect()
            },
            plain: |bars| {
                let mut p = $Ty::default();
                bars.iter().map(|b| p.next_ohlc(b.open, b.high, b.low, b.close)).collect()
            },
            on_bar: |bars| {
                let mut p = $Ty::default();
                bars.iter().map(|b| p.on_bar(b)[0]).collect()
            },
            vectorize: |bars| $Ty::default().vectorize(bars).remove(0),
            bull: $bull,
            bear: $bear,
        }
    };
}

fn cases() -> Vec<Case> {
    vec![
        case!(Engulfing, "engulfing", batch_engulfing, engulfing, bull = true, bear = true),
        case!(DojiStar, "doji_star", batch_doji_star, doji_star, bull = true, bear = true),
        case!(Harami, "harami", batch_harami, harami, bull = true, bear = true),
        case!(
            HaramiCross,
            "harami_cross",
            batch_harami_cross,
            harami_cross,
            bull = true,
            bear = true
        ),
        case!(Piercing, "piercing", batch_piercing, piercing, bull = true, bear = false),
        case!(
            DarkCloudCover,
            "dark_cloud_cover",
            batch_dark_cloud_cover,
            dark_cloud_cover,
            bull = false,
            bear = true
        ),
        case!(
            Counterattack,
            "counterattack",
            batch_counterattack,
            counterattack,
            bull = true,
            bear = true
        ),
        case!(
            MeetingLines,
            "meeting_lines",
            batch_meeting_lines,
            meeting_lines,
            bull = true,
            bear = true
        ),
        case!(
            SeparatingLines,
            "separating_lines",
            batch_separating_lines,
            separating_lines,
            bull = true,
            bear = true
        ),
        case!(
            MatchingLow,
            "matching_low",
            batch_matching_low,
            matching_low,
            bull = true,
            bear = false
        ),
        case!(OnNeck, "on_neck", batch_on_neck, on_neck, bull = false, bear = true),
        case!(InNeck, "in_neck", batch_in_neck, in_neck, bull = false, bear = true),
        case!(Thrusting, "thrusting", batch_thrusting, thrusting, bull = false, bear = true),
        case!(Kicking, "kicking", batch_kicking, kicking, bull = true, bear = true),
        case!(
            KickingByLength,
            "kicking_by_length",
            batch_kicking_by_length,
            kicking_by_length,
            bull = true,
            bear = true
        ),
        case!(
            HomingPigeon,
            "homing_pigeon",
            batch_homing_pigeon,
            homing_pigeon,
            bull = true,
            bear = false
        ),
        case!(
            GapSideSideWhite,
            "gap_side_side_white",
            batch_gap_side_side_white,
            gap_side_side_white,
            bull = true,
            bear = false
        ),
        case!(TasukiGap, "tasuki_gap", batch_tasuki_gap, tasuki_gap, bull = true, bear = true),
    ]
}

fn bits(v: &[f64]) -> Vec<u64> {
    v.iter().map(|x| x.to_bits()).collect()
}

/// Every way in, against the old kernel, over `bars`.
fn check_all_ways(c: &Case, bars: &[Bar], what: &str) {
    let want = bits(&(c.old)(bars).remove(0));
    let (o, h, l, cl): (Vec<f64>, Vec<f64>, Vec<f64>, Vec<f64>) = (
        bars.iter().map(|b| b.open).collect(),
        bars.iter().map(|b| b.high).collect(),
        bars.iter().map(|b| b.low).collect(),
        bars.iter().map(|b| b.close).collect(),
    );
    for (way, got) in [
        ("next", (c.stream)(bars)),
        ("next_ohlc", (c.plain)(bars)),
        ("on_bar", (c.on_bar)(bars)),
        ("vectorize", (c.vectorize)(bars)),
        ("series", (c.series)(&o, &h, &l, &cl)),
    ] {
        let got = bits(&got);
        assert_eq!(got.len(), want.len(), "{} {what}: {way} length", c.name);
        if let Some(i) = (0..got.len()).find(|&i| got[i] != want[i]) {
            panic!(
                "{} {what}: {way} differs from the old kernel at bar {i}: {:?} against {:?}",
                c.name,
                f64::from_bits(got[i]),
                f64::from_bits(want[i])
            );
        }
    }
}

#[test]
fn every_way_in_equals_the_old_kernel_on_three_scales_and_every_length() {
    for c in cases() {
        let (mut bull, mut bear) = (0usize, 0usize);
        for (k, &(level, step)) in SCALES.iter().enumerate() {
            let bars = rich(level, step, 2600, 0x9e37_79b9_7f4a_7c15 ^ (k as u64 + 1));
            for &len in &LENGTHS {
                check_all_ways(&c, &bars[..len], &format!("scale {level} length {len}"));
            }
            for x in (c.old)(&bars).remove(0) {
                if x == 100.0 {
                    bull += 1;
                } else if x == -100.0 {
                    bear += 1;
                } else {
                    assert_eq!(
                        x.to_bits(),
                        0.0f64.to_bits(),
                        "{}: the old kernel gave {x}",
                        c.name
                    );
                }
            }
        }
        println!("{:<22} +100: {bull:>5}   -100: {bear:>5}", c.name);
        assert_eq!(c.bull, bull > 0, "{}: +100 signals ({bull}) against what it can emit", c.name);
        assert_eq!(c.bear, bear > 0, "{}: -100 signals ({bear}) against what it can emit", c.name);
        // 3 scales x 2600 bars: a pattern that fired a handful of times proves little.
        assert!(bull + bear >= 20, "{}: only {} signals on 7,800 bars", c.name, bull + bear);
    }
}

#[test]
fn a_nan_an_infinity_or_a_negative_zero_in_any_price_matches_the_old_kernel() {
    let base = rich(100.0, 2.0, 80, 0xD1B5_4A32_D192_ED03);
    for c in cases() {
        for at in [0usize, 1, 2, 5, 9, 10, 11, 19, 20, 30, 79] {
            for field in 0..4 {
                for poison in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.0] {
                    let mut bars = base.clone();
                    let b = &mut bars[at];
                    match field {
                        0 => b.open = poison,
                        1 => b.high = poison,
                        2 => b.low = poison,
                        _ => b.close = poison,
                    }
                    check_all_ways(&c, &bars, &format!("{poison} in field {field} at bar {at}"));
                }
            }
        }
    }
}

#[test]
fn flat_and_constant_series_match_the_old_kernel() {
    // Zero ranges, zero bodies, a zero average body (every average-body pattern then answers 0),
    // and bars that are all the same price.
    let flat: Vec<Bar> =
        (0..200).map(|i| Bar::ohlcv(i * 60_000, 5.0, 5.0, 5.0, 5.0, 1.0)).collect();
    let zeros: Vec<Bar> =
        (0..200).map(|i| Bar::ohlcv(i * 60_000, 0.0, 0.0, 0.0, 0.0, 0.0)).collect();
    let mut stairs = rich(100.0, 2.0, 200, 7);
    for (i, b) in stairs.iter_mut().enumerate().skip(100).take(40) {
        (b.open, b.high, b.low, b.close) = (50.0, 50.0, 50.0, 50.0 + (i % 2) as f64);
    }
    for c in cases() {
        for (what, bars) in [("flat", &flat), ("zeros", &zeros), ("stairs", &stairs)] {
            check_all_ways(&c, bars, what);
        }
    }
}

#[test]
fn slices_of_unequal_length_give_the_shortest() {
    let bars = rich(100.0, 2.0, 60, 11);
    let o: Vec<f64> = bars.iter().map(|b| b.open).collect();
    let h: Vec<f64> = bars.iter().map(|b| b.high).collect();
    let l: Vec<f64> = bars.iter().map(|b| b.low).collect();
    let cl: Vec<f64> = bars.iter().map(|b| b.close).collect();
    for c in cases() {
        let full = bits(&(c.series)(&o, &h, &l, &cl));
        assert_eq!(full.len(), 60);
        for (a, b, d, e) in [(40, 60, 60, 60), (60, 33, 60, 60), (60, 60, 47, 60), (60, 60, 60, 12)]
        {
            let got = bits(&(c.series)(&o[..a], &h[..b], &l[..d], &cl[..e]));
            let n = a.min(b).min(d).min(e);
            assert_eq!(got, full[..n], "{}: lengths {a} {b} {d} {e}", c.name);
        }
        assert!((c.series)(&[], &h, &l, &cl).is_empty());
    }
}

#[test]
fn the_registry_instance_by_name_equals_the_typed_one() {
    let bars = rich(100.0, 2.0, 600, 3);
    for c in cases() {
        let want = bits(&(c.old)(&bars).remove(0));
        let mut ind = vike_indicators::make(c.name).expect("a registry name");
        let got: Vec<f64> = bars.iter().map(|b| ind.on_bar(b)[0]).collect();
        assert_eq!(bits(&got), want, "{}", c.name);
        let batch = ind.vectorize(&bars).remove(0);
        assert_eq!(bits(&batch), want, "{} (vectorize)", c.name);
        let named = vike_indicators::compute(c.name, &[], &bars).expect("a registry name");
        assert_eq!(bits(&named[0].1), want, "{} (compute)", c.name);
    }
}

#[test]
fn reset_value_and_clone_behave_like_every_other_stepper() {
    let bars = rich(100.0, 2.0, 300, 5);
    for c in cases() {
        let want = (c.old)(&bars).remove(0);
        let mut ind = vike_indicators::make(c.name).unwrap();
        assert!(ind.value()[0].is_nan(), "{}: value() before any bar", c.name);
        for (i, b) in bars.iter().enumerate().take(150) {
            let v = ind.on_bar(b)[0];
            assert_eq!(v.to_bits(), want[i].to_bits());
            assert_eq!(ind.value()[0].to_bits(), v.to_bits(), "{}: value() after bar {i}", c.name);
        }
        // A clone made mid-stream carries on identically; the original is not disturbed by it.
        let mut twin = ind.clone_box();
        for (i, b) in bars.iter().enumerate().skip(150) {
            assert_eq!(twin.on_bar(b)[0].to_bits(), want[i].to_bits(), "{} clone", c.name);
        }
        // After `reset` the stepper starts over: same outputs from bar 0, `value()` NaN again.
        ind.reset();
        assert!(ind.value()[0].is_nan(), "{}: value() after reset", c.name);
        for (i, b) in bars.iter().enumerate() {
            assert_eq!(ind.on_bar(b)[0].to_bits(), want[i].to_bits(), "{} after reset", c.name);
        }
    }
}

#[test]
fn the_warm_up_contract_is_unchanged() {
    // The old streamers kept the defaults: `lookback() == 0`, not exact (a pattern has no derivable
    // warm-up), not path dependent. The first bar has no bar before it and answers `0.0`.
    for c in cases() {
        let ind = vike_indicators::make(c.name).unwrap();
        assert_eq!(ind.lookback(), 0, "{}", c.name);
        assert_eq!(ind.lookback_full(), 0, "{}", c.name);
        assert!(!ind.lookback_exact(), "{}", c.name);
        assert!(!ind.warmup_path_dependent(), "{}", c.name);
        assert!(!ind.trims_history(), "{}: bounded state keeps no history", c.name);
        assert_eq!(ind.name(), c.name);
        let bars = rich(100.0, 2.0, 5, 1);
        assert_eq!((c.stream)(&bars)[0].to_bits(), 0.0f64.to_bits(), "{}: first bar", c.name);
    }
}
