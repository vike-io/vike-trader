//! A parameter given on each bar (I8): `supertrend`'s multiplier (`Supertrend::next_centred_with_mult`) and its bands carried
//! on the trend (`Supertrend::with_carry_on_trend`), `smma`'s period (`Smma::next_value_with_period`) and `hma`'s
//! (`Hma::next_value_with_period`). Each against the law `strategy_lab_b` wrote out, written out here again; with the type's
//! own parameter on every bar each call IS the fixed one; the option is off by default (the registry row's numbers), and the
//! stream under it equals `vectorize` and survives `reset`.

use vike_indicators::indicators::{Hma, Smma, Supertrend};
// The rule's `max`/`min`: `NaN` when either is.
use vike_indicators::strategy_kit::{max, min};
use vike_marketdata::Bar;

use crate::fast::{assert_close, closes, col, same};
use crate::options::{assert_lines_same, checked_stream, hourly_bars, registry};
use crate::options_b::{st_atr, st_sets, st_skip_atr, wave_two_sets};

/// A multiplier that moves every bar, from 0.6 to 4.65.
fn moving_multiplier(n: usize) -> Vec<f64> {
    (0..n).map(|i| 0.6 + ((i * 7) % 10) as f64 * 0.45).collect()
}

// ------------------------------------------------------------------------------ supertrend

/// `supertrend_ai_strategy_adaptive`'s and `supertrend_regime_confluence`'s one-band Supertrend: up at first; the band the
/// trend holds is carried (`max` while up, `min` while down), and a close past it turns the trend and restarts the band from
/// this bar's other one. `(band, up)`.
struct PbOneBand {
    band: f64,
    up: bool,
}

impl PbOneBand {
    fn new() -> Self {
        PbOneBand { band: f64::NAN, up: true }
    }

    fn next(&mut self, close: f64, centre: f64, atr: f64, mult: f64) -> (f64, bool) {
        let (upper, lower) = (centre + mult * atr, centre - mult * atr);
        let previous =
            if self.band.is_nan() { if self.up { lower } else { upper } } else { self.band };
        if self.up {
            self.band = max(lower, previous);
            if close < self.band {
                (self.up, self.band) = (false, upper);
            }
        } else {
            self.band = min(upper, previous);
            if close > self.band {
                (self.up, self.band) = (true, lower);
            }
        }
        (self.band, self.up)
    }
}

/// `vegas_supertrend_enhanced_strategy_presenttradin`'s Supertrend: the close judged against the last bar's bands (this bar's
/// when the last bar had none), up at first; after the turn the lower band is carried while the trend is up, the upper while it
/// is down. `(line, direction)`.
struct PbTrendCarry {
    below: f64,
    above: f64,
    trend: f64,
}

impl PbTrendCarry {
    fn new() -> Self {
        PbTrendCarry { below: f64::NAN, above: f64::NAN, trend: 1.0 }
    }

    fn next(&mut self, close: f64, centre: f64, atr: f64, mult: f64) -> (f64, f64) {
        let (below, above) = (centre - mult * atr, centre + mult * atr);
        let last_below = if self.below.is_nan() { below } else { self.below };
        let last_above = if self.above.is_nan() { above } else { self.above };
        self.trend = if close > last_above {
            1.0
        } else if close < last_below {
            -1.0
        } else {
            self.trend
        };
        self.below = if self.trend == 1.0 { max(below, last_below) } else { below };
        self.above = if self.trend == -1.0 { min(above, last_above) } else { above };
        (if self.trend == 1.0 { self.below } else { self.above }, self.trend)
    }
}

/// `next_centred_with_mult` over the given numbers, `[line, direction]` a bar.
fn pb_with_multiplier(
    mut indicator: Supertrend,
    b: &[Bar],
    centre: &[f64],
    atr: &[f64],
    mult: &[f64],
) -> [Vec<f64>; 2] {
    let rows: Vec<[f64; 2]> = (0..b.len())
        .map(|i| indicator.next_centred_with_mult(b[i].close, centre[i], atr[i], mult[i]))
        .collect();
    [col(&rows, 0), col(&rows, 1)]
}

/// The bars' midpoints.
fn mids(b: &[Bar]) -> Vec<f64> {
    b.iter().map(|bar| (bar.high + bar.low) * 0.5).collect()
}

/// With the type's own multiplier on every bar, `next_centred_with_mult` IS `next_centred`, bit for bit, in every form.
#[test]
fn pb_supertrend_multiplier_of_the_type_is_next_centred() {
    for (name, b) in &st_sets() {
        let (mid, atr) = (mids(b), st_atr(b, 10.0));
        for factor in [3.0, 0.5, 7.5] {
            let base = || Supertrend::new(10.0, factor).unwrap();
            let forms = [
                ("registry", base()),
                ("previous bands", base().with_previous_bands(true)),
                ("gated", base().with_previous_bands(true).with_gated_turns(true)),
                ("down start", base().with_down_start(true)),
                ("carry on trend", base().with_carry_on_trend(true)),
                ("previous, on trend", base().with_previous_bands(true).with_carry_on_trend(true)),
            ];
            for (form, indicator) in forms {
                let tag = format!("{name} supertrend(10, {factor}) {form}");
                let mut fixed = indicator.clone();
                let rows: Vec<[f64; 2]> = b
                    .iter()
                    .zip(&mid)
                    .zip(&atr)
                    .map(|((bar, &m), &a)| fixed.next_centred(bar.close, m, a))
                    .collect();
                let got = pb_with_multiplier(indicator, b, &mid, &atr, &vec![factor; b.len()]);
                assert_lines_same(&tag, &got, &[col(&rows, 0), col(&rows, 1)]);
            }
        }
    }
}

/// Set, `carry_on_trend` IS the one-band Supertrend bit for bit: over bars (the registry row's ATR about the midpoint) and
/// through `next_centred_with_mult` with the `atr` row's ATR and a multiplier that moves each bar, the line where the rule has a
/// band and the direction from the first bar (`NaN` reads as the rule's start, up). Under `down_start` the stream still equals
/// `vectorize` and survives `reset`. Unset, the registry row; set, it moves.
#[test]
fn pb_supertrend_carry_on_trend_is_the_one_band_supertrend() {
    let mut differs = false;
    for (name, b) in &st_sets() {
        let mid = mids(b);
        let mult = moving_multiplier(b.len());
        for (n, factor) in [(10.0, 3.0), (1.0, 0.5), (14.0, 2.0)] {
            let tag = format!("{name} supertrend({n}, {factor}) carry on trend");
            let set = Supertrend::new(n, factor).unwrap().with_carry_on_trend(true);
            let got = checked_stream(&tag, Box::new(set.clone()), b);
            differs |= !same(&got[0], &registry("supertrend", &[n, factor], b)[0]);
            let check = |got: &[Vec<f64>], atr: &[f64], mult: &[f64], tag: &str| {
                let mut lab = PbOneBand::new();
                for (i, bar) in b.iter().enumerate() {
                    let (band, up) = lab.next(bar.close, mid[i], atr[i], mult[i]);
                    assert!(same(&[got[0][i]], &[band]), "{tag}: line at {i}");
                    let dir = got[1][i];
                    assert_eq!(dir.is_nan() || dir > 0.0, up, "{tag}: direction at {i}");
                }
            };
            check(&got, &st_skip_atr(b, n), &vec![factor; b.len()], &tag);
            let atr = st_atr(b, n);
            let moved = pb_with_multiplier(set, b, &mid, &atr, &mult);
            check(&moved, &atr, &mult, &format!("{tag}, multiplier per bar"));
            let down = Supertrend::new(n, factor).unwrap().with_down_start(true);
            checked_stream(&tag, Box::new(down.with_carry_on_trend(true)), b);
        }
    }
    assert!(differs, "carry on trend changed nothing");
    let b = hourly_bars();
    let row = registry("supertrend", &[10.0, 3.0], &b);
    let off = Supertrend::new(10.0, 3.0).unwrap().with_carry_on_trend(false);
    assert_lines_same("carry on trend off", &checked_stream("off", Box::new(off), &b), &row);
}

/// Set under `previous_bands`, `carry_on_trend` IS the trend-carried Supertrend bit for bit, both lines from the first bar:
/// over bars (the `atr` row's ATR about the midpoint), and through `next_centred_with_mult` about the typical price with a
/// multiplier missing for the first 99 bars, as the channel-width one is, then moving each bar. The stream equals
/// `vectorize` and survives `reset`; `carry_on_close` and `gated_turns` read nothing under it.
#[test]
fn pb_supertrend_carry_on_trend_under_previous_bands_is_the_trend_carried_supertrend() {
    let mut differs = false;
    for (name, b) in &st_sets() {
        let (mid, typical): (Vec<f64>, Vec<f64>) =
            b.iter().map(|x| ((x.high + x.low) * 0.5, (x.high + x.low + x.close) / 3.0)).unzip();
        let mut mult = moving_multiplier(b.len());
        mult[..99].fill(f64::NAN);
        for (n, factor) in [(10.0, 3.0), (1.0, 0.5), (14.0, 5.5)] {
            let tag = format!("{name} supertrend({n}, {factor}) previous bands, carry on trend");
            let previous = Supertrend::new(n, factor).unwrap().with_previous_bands(true);
            let plain = checked_stream(&tag, Box::new(previous.clone()), b);
            let set = previous.with_carry_on_trend(true);
            let got = checked_stream(&tag, Box::new(set.clone()), b);
            differs |= !same(&plain[1], &got[1]);
            let atr = st_atr(b, n);
            let mut lab = PbTrendCarry::new();
            let (line, dir) =
                (0..b.len()).map(|i| lab.next(b[i].close, mid[i], atr[i], factor)).unzip();
            assert_lines_same(&tag, &got, &[line, dir]);
            let mut lab = PbTrendCarry::new();
            let (line, dir) =
                (0..b.len()).map(|i| lab.next(b[i].close, typical[i], atr[i], mult[i])).unzip();
            let moved = pb_with_multiplier(set.clone(), b, &typical, &atr, &mult);
            assert_lines_same(&format!("{tag}, multiplier per bar"), &moved, &[line, dir]);
            let others = set.with_gated_turns(true).with_carry_on_close(true);
            assert_lines_same(
                &format!("{tag}, gated, on close"),
                &checked_stream(&tag, Box::new(others), b),
                &got,
            );
        }
    }
    assert!(differs, "carry on trend changed nothing under previous bands");
}

// ------------------------------------------------------------------------------ smma

/// `dynamic_rsi_momentum`'s Wilder average whose length may change every value: the last `most` values kept until the seed,
/// the seed the plain mean of the last `len` on the first value that has `len`, then `x / len + (1 - 1 / len) * previous`.
fn pb_lab_var_rma(x: &[f64], len: &[usize], most: usize) -> Vec<f64> {
    let (mut seen, mut value) = (std::collections::VecDeque::new(), f64::NAN);
    x.iter()
        .zip(len)
        .map(|(&x, &len)| {
            if value.is_nan() {
                seen.push_back(x);
                if seen.len() > most {
                    seen.pop_front();
                }
                if seen.len() >= len {
                    value = seen.iter().skip(seen.len() - len).fold(0.0, |s, v| s + v) / len as f64;
                }
            } else {
                let alpha = 1.0 / len as f64;
                value = alpha * x + (1.0 - alpha) * value;
            }
            value
        })
        .collect()
}

/// Under `alpha_steps`, `next_value_with_period` IS the lab's variable-length Wilder average bit for bit, over closes and over
/// the up moves an RSI feeds it, with lengths from 20 to 50 that move each value (one run that starts over its first lengths'
/// reach, one that shrinks before the seed). With the type's period on every value it IS `next_value` in every form, a `NaN`
/// value included.
#[test]
fn pb_smma_period_per_value_is_the_labs_variable_rma() {
    for (name, b) in wave_two_sets() {
        let c: Vec<f64> = b.iter().map(|x| x.close).collect();
        let ups: Vec<f64> = std::iter::once(0.0)
            .chain(c.windows(2).map(|w| if w[1] > w[0] { w[1] - w[0] } else { 0.0 }))
            .collect();
        let swing: Vec<usize> = (0..c.len()).map(|i| 20 + (i * 13) % 31).collect();
        let shrink: Vec<usize> =
            (0..c.len()).map(|i| if i < 30 { 50 - i } else { 20 + (i / 40) % 31 }).collect();
        for (series, x) in [("closes", &c), ("up moves", &ups)] {
            for (how, len) in [("swinging", &swing), ("shrinking", &shrink)] {
                let tag = format!("{name}: smma(50) alpha steps, {how} periods over the {series}");
                let mut indicator = Smma::new(50.0).unwrap().with_alpha_steps(true);
                let got: Vec<f64> = x
                    .iter()
                    .zip(len)
                    .map(|(&v, &n)| indicator.next_value_with_period(v, n as f64))
                    .collect();
                assert!(same(&got, &pb_lab_var_rma(x, len, 50)), "{tag}");
            }
        }
        let mut gapped = c.clone();
        gapped[30] = f64::NAN;
        for n in [2.0, 14.0, 50.0] {
            let base = || Smma::new(n).unwrap();
            let forms = [
                ("plain", base()),
                ("alpha steps", base().with_alpha_steps(true)),
                ("seed first", base().with_seed_first(true)),
                ("zero seed", base().with_zero_seed(true).with_alpha_steps(true)),
                ("skip nan", base().with_skip_nan(true)),
            ];
            for (form, indicator) in forms {
                for (series, x) in [("closes", &c), ("closes with a gap", &gapped)] {
                    let tag = format!("{name}: smma({n}) {form}, its own period over the {series}");
                    let mut fixed = indicator.clone();
                    let want: Vec<f64> = x.iter().map(|&v| fixed.next_value(v)).collect();
                    let mut moving = indicator.clone();
                    let got: Vec<f64> =
                        x.iter().map(|&v| moving.next_value_with_period(v, n)).collect();
                    assert!(same(&got, &want), "{tag}");
                }
            }
        }
    }
}

// ------------------------------------------------------------------------------ hma

/// `s_72s_strategy_adaptive_hull_moving_average_pt_1`'s Hull whose length may change every bar, as the rule writes its
/// averages: each a fresh weighted average with weights `length` down to 1 times `length`, `NaN` while fewer values are in,
/// the first stage over this bar's `length / 2` and `length`, the second over its floored root.
fn pb_lab_hull(c: &[f64], length: &[usize]) -> Vec<f64> {
    let wma = |v: &[f64], n: usize| {
        if n == 0 || v.len() < n {
            return f64::NAN;
        }
        let (mut norm, mut sum) = (0.0, 0.0);
        for i in 0..n {
            let weight = ((n - i) * n) as f64;
            norm += weight;
            sum += v[v.len() - 1 - i] * weight;
        }
        sum / norm
    };
    let mut inner = Vec::new();
    (0..c.len())
        .map(|i| {
            let n = length[i];
            inner.push(2.0 * wma(&c[..=i], n / 2) - wma(&c[..=i], n));
            wma(&inner, (n as f64).sqrt().floor() as usize)
        })
        .collect()
}

/// `next_value_with_period` IS the lab's adaptive Hull to 1e-12 (the rule weighs by `length` times the textbook weights, so
/// the last bits differ), `NaN` exactly where it has one: lengths that shrink and grow 3.141% a bar between 172 and 233, as
/// the rule's major average, and between 89 and 121; and a length that jumps every bar. With the type's period on every close
/// it is `next_value` to 1e-9 (the tolerance this crate holds its running-sum Hull to against a fresh fold), `NaN` in the
/// same places, in every form; `reset` forgets the windows.
#[test]
fn pb_hma_period_per_close_is_the_labs_adaptive_hull() {
    for (name, b) in wave_two_sets() {
        let c: Vec<f64> = b.iter().map(|x| x.close).collect();
        for (low, high) in [(172.0, 233.0), (89.0, 121.0)] {
            let mut length = (low + high) / 2.0;
            let lengths: Vec<usize> = (0..c.len())
                .map(|i| {
                    length = if (i / 37) % 2 == 0 {
                        f64::max(low, length * (1.0 - 0.03141))
                    } else {
                        f64::min(high, length * (1.0 + 0.03141))
                    };
                    length as usize
                })
                .collect();
            let tag = format!("{name}: hma({high}), adaptive from {low}");
            let mut indicator = Hma::new(high).unwrap();
            let got: Vec<f64> = c
                .iter()
                .zip(&lengths)
                .map(|(&x, &n)| indicator.next_value_with_period(x, n as f64))
                .collect();
            assert_close(&tag, &got, &pb_lab_hull(&c, &lengths), 1e-12, 1e-300);
        }
        let jumps: Vec<usize> = (0..c.len()).map(|i| 4 + (i * 11) % 47).collect();
        let mut indicator = Hma::new(50.0).unwrap();
        let got: Vec<f64> = c
            .iter()
            .zip(&jumps)
            .map(|(&x, &n)| indicator.next_value_with_period(x, n as f64))
            .collect();
        assert_close(
            &format!("{name}: hma(50), jumping"),
            &got,
            &pb_lab_hull(&c, &jumps),
            1e-12,
            1e-300,
        );
        for n in [2.0, 3.0, 20.0, 55.0] {
            let base = || Hma::new(n).unwrap();
            let forms = [
                ("plain", base()),
                ("rounded root", base().with_rounded_root(true)),
                ("rounded half", base().with_rounded_half(true)),
                ("unit lengths", base().with_unit_lengths(true)),
            ];
            for (form, indicator) in forms {
                let tag = format!("{name}: hma({n}) {form}, its own period");
                let mut fixed = indicator.clone();
                let want: Vec<f64> = c.iter().map(|&x| fixed.next_value(x)).collect();
                let mut moving = indicator;
                let got: Vec<f64> =
                    c.iter().map(|&x| moving.next_value_with_period(x, n)).collect();
                assert_close(&tag, &got, &want, 1e-9, 1e-300);
            }
        }
    }
    let c = closes(100.0, 2.0, 300);
    let mut indicator = Hma::new(20.0).unwrap();
    let first: Vec<f64> = c.iter().map(|&x| indicator.next_value_with_period(x, 15.0)).collect();
    vike_indicators::Indicator::reset(&mut indicator);
    let again: Vec<f64> = c.iter().map(|&x| indicator.next_value_with_period(x, 15.0)).collect();
    assert!(same(&first, &again), "hma: reset forgets the per-close windows");
}

/// `Hma::next_value_with_period` as it was before its running sums, under the default options: every window folded
/// afresh on each close, oldest first from `0.0` with weights `1, 2, …`, times `vike_math`'s normaliser, each combined
/// value kept as its close folded it; `half = n / 2` and `root = floor(sqrt(n))`, each at least 2.
fn pb_cold_hull(c: &[f64], length: &[usize]) -> Vec<f64> {
    let fold = |v: &[f64]| v.iter().enumerate().fold(0.0, |w, (k, &x)| w + (k + 1) as f64 * x);
    let normaliser = vike_math::smoothing::weighted_moving_average_normaliser;
    let mut combined = Vec::new();
    (0..c.len())
        .map(|i| {
            let (n, seen) = (length[i], &c[..=i]);
            let (half, root) = ((n / 2).max(2), ((n as f64).sqrt() as usize).max(2));
            combined.push(if seen.len() < n {
                f64::NAN
            } else {
                2.0 * (fold(&seen[seen.len() - half..]) * normaliser(half))
                    - fold(&seen[seen.len() - n..]) * normaliser(n)
            });
            let tail = &combined[combined.len().saturating_sub(root)..];
            if tail.len() < root { f64::NAN } else { fold(tail) * normaliser(root) }
        })
        .collect()
}

/// A `NaN` and an infinity in the closes, the lengths moving every close before, across and after them: wherever the
/// cold fold answers a `NaN` or an infinity, and on the first close it answers a number again, the running sums are off
/// and the answer is the cold fold's to the bit (`NaN` exactly where it has one); everywhere else it is the running sums'
/// within 1e-12.
#[test]
fn pb_hma_period_per_close_falls_back_to_the_cold_fold_around_a_nan_and_an_infinity() {
    let mut c = closes(100.0, 2.0, 600);
    (c[200], c[330]) = (f64::NAN, f64::INFINITY);
    // 28 up to 36 and back, one a close: every window moves at most a quarter, so the sums carry the change.
    let lengths: Vec<usize> = (0..c.len()).map(|i| 28 + (i % 16).min(16 - i % 16)).collect();
    let mut indicator = Hma::new(40.0).unwrap();
    let got: Vec<f64> = c
        .iter()
        .zip(&lengths)
        .map(|(&x, &n)| indicator.next_value_with_period(x, n as f64))
        .collect();
    let want = pb_cold_hull(&c, &lengths);
    assert!(want.contains(&f64::NEG_INFINITY), "the infinity reaches the line as one");
    let mut cold = false;
    for (i, (&g, &w)) in got.iter().zip(&want).enumerate() {
        assert_eq!(g.is_nan(), w.is_nan(), "hma[{i}]: got {g}, want {w}: NaN in one only");
        if cold || !w.is_finite() {
            assert!(
                w.is_nan() || g.to_bits() == w.to_bits(),
                "hma[{i}]: got {g}, want {w}: the cold fold's bits"
            );
        } else {
            let err = (g - w).abs() / w.abs().max(1e-300);
            assert!(err <= 1e-12, "hma[{i}]: got {g}, want {w}, relative error {err:e}");
        }
        cold = !w.is_finite();
    }
}
