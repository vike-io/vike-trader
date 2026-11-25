//! The typed options (`with_*`) of `ema`, `smma`, `sma`, `vwap`, `psar`, `kama`, `adx`, `keltner` and
//! `rank_correlation`. Each is off by default, so a type that never calls one answers exactly what
//! its registry row answers (the `_unset_` tests; across commits, `tests/vectorize_hash.rs` is the
//! proof that the rows themselves did not move). Set, each gives the definition its doc names, held
//! here bit for bit to that definition written out plainly, in the strategies' own arithmetic where
//! the crate's is the same, and to hand-worked values where the arithmetic is exact.
//!
//! Every option also keeps the crate's two contracts: the stream equals `vectorize` (the option is in
//! the clone `vectorize` folds), and `reset` keeps it.

use std::collections::VecDeque;

use vike_indicators::indicators::{
    Adx, Ema, Kama, Keltner, Psar, RankCorrelation, Sma, Smma, TrueRange, Vwap, VwapSource,
};
use vike_indicators::{Indicator, make_with};
use vike_marketdata::Bar;

use crate::fast::{XorShift, closes, same, spread_bars};

// ---------------------------------------------------------------- helpers

/// `indicator` folded one bar at a time, one `Vec` per line.
fn stream(indicator: &mut dyn Indicator, bars: &[Bar]) -> Vec<Vec<f64>> {
    let rows: Vec<Vec<f64>> = bars.iter().map(|b| indicator.on_bar(b)).collect();
    let lines = rows.first().map_or(0, Vec::len);
    (0..lines).map(|k| rows.iter().map(|r| r[k]).collect()).collect()
}

pub(crate) fn assert_lines_same(name: &str, got: &[Vec<f64>], want: &[Vec<f64>]) {
    assert_eq!(got.len(), want.len(), "{name}: line count");
    for (k, (g, w)) in got.iter().zip(want).enumerate() {
        assert!(same(g, w), "{name}: line {k} differs");
    }
}

/// The stream of `indicator` over `bars`, after checking it equals `vectorize` and survives a `reset`.
pub(crate) fn checked_stream(
    name: &str,
    mut indicator: Box<dyn Indicator>,
    bars: &[Bar],
) -> Vec<Vec<f64>> {
    let batch = indicator.vectorize(bars);
    let first = stream(indicator.as_mut(), bars);
    indicator.reset();
    let again = stream(indicator.as_mut(), bars);
    assert_lines_same(&format!("{name}: stream vs vectorize"), &first, &batch);
    assert_lines_same(&format!("{name}: stream after reset"), &again, &first);
    first
}

/// The registry row `key` at `params` over `bars`: what an unset option must answer.
pub(crate) fn registry(key: &str, params: &[f64], bars: &[Bar]) -> Vec<Vec<f64>> {
    make_with(key, params).expect("a registry row").vectorize(bars)
}

fn bars() -> Vec<Bar> {
    spread_bars(&closes(100.0, 2.0, 1500))
}

pub(crate) fn bits(x: f64) -> u64 {
    if x.is_nan() { f64::NAN.to_bits() } else { x.to_bits() }
}

// ---------------------------------------------------------------- ema

#[test]
fn ema_options_unset_are_the_registry_row() {
    let b = bars();
    for n in [1.0, 3.0, 20.0, 200.0] {
        let want = registry("ema", &[n], &b);
        let plain = checked_stream("ema", Box::new(Ema::new(n).unwrap()), &b);
        let off = Ema::new(n).unwrap().with_skip_nan(false).with_seed_first(false);
        assert_lines_same("ema", &plain, &want);
        assert_lines_same("ema, options off", &checked_stream("ema off", Box::new(off), &b), &want);
    }
}

/// `seed_first` is `pivot_anchored_bracket`'s `FromFirst`: the first close, then
/// `speed * close + (1 - speed) * value`.
#[test]
fn ema_seed_first_is_the_average_seeded_with_the_first_value() {
    let c = closes(100.0, 2.0, 500);
    for n in [5.0, 13.0, 1.0] {
        let speed = 2.0 / (n + 1.0);
        let mut value = f64::NAN;
        let mut e = Ema::new(n).unwrap().with_seed_first(true);
        for (i, &x) in c.iter().enumerate() {
            value = if value.is_nan() { x } else { speed * x + (1.0 - speed) * value };
            assert_eq!(bits(e.next_value(x)), bits(value), "ema({n}) seed_first, bar {i}");
        }
        assert_eq!(e.lookback(), 0);
    }
    let b = bars();
    checked_stream("ema seed_first", Box::new(Ema::new(13.0).unwrap().with_seed_first(true)), &b);
}

/// `skip_nan` is `rsi_ema_signal_cross`'s guard: only defined values are fed, and a `NaN` bar reads
/// `NaN`. Unset, one `NaN` poisons the line for good.
#[test]
fn ema_skip_nan_drops_a_nan_and_carries_on() {
    let mut c = closes(100.0, 2.0, 300);
    for at in [0, 1, 7, 50, 51, 120] {
        c[at] = f64::NAN;
    }
    for seed_first in [false, true] {
        let mut skip = Ema::new(9.0).unwrap().with_skip_nan(true).with_seed_first(seed_first);
        let mut fed = Ema::new(9.0).unwrap().with_seed_first(seed_first);
        let mut plain = Ema::new(9.0).unwrap();
        for (i, &x) in c.iter().enumerate() {
            let want = if x.is_nan() { f64::NAN } else { fed.next_value(x) };
            assert_eq!(bits(skip.next_value(x)), bits(want), "seed_first {seed_first}, bar {i}");
            assert!(plain.next_value(x).is_nan(), "unset, the NaN at bar 0 poisons every value");
        }
        assert!(skip.next_value(101.0).is_finite());
    }
}

// ---------------------------------------------------------------- smma

#[test]
fn smma_options_unset_are_the_registry_row() {
    let b = bars();
    for n in [2.0, 14.0, 400.0] {
        let want = registry("smma", &[n], &b);
        let plain = checked_stream("smma", Box::new(Smma::new(n).unwrap()), &b);
        let off = Smma::new(n).unwrap().with_skip_nan(false).with_seed_first(false);
        assert_lines_same("smma", &plain, &want);
        assert_lines_same(
            "smma, options off",
            &checked_stream("smma off", Box::new(off), &b),
            &want,
        );
    }
}

/// `seed_first`: the first close, then Wilder's `value * (n - 1) / n + close / n`, written as the
/// crate writes it (`previous * keep + x * gain`).
#[test]
fn smma_seed_first_is_wilders_average_seeded_with_the_first_value() {
    let c = closes(100.0, 2.0, 500);
    for n in [2.0_f64, 14.0] {
        let (keep, gain) = ((n - 1.0) / n, 1.0 / n);
        let mut value = f64::NAN;
        let mut s = Smma::new(n).unwrap().with_seed_first(true);
        for (i, &x) in c.iter().enumerate() {
            value = if value.is_nan() { x } else { value * keep + x * gain };
            assert_eq!(bits(s.next_value(x)), bits(value), "smma({n}) seed_first, bar {i}");
        }
    }
    let b = bars();
    checked_stream("smma seed_first", Box::new(Smma::new(14.0).unwrap().with_seed_first(true)), &b);
}

/// `skip_nan` is `meridian_dmi_regime`'s guard: nothing is fed until the input has a value.
#[test]
fn smma_skip_nan_drops_a_nan_and_carries_on() {
    let mut c = closes(100.0, 2.0, 300);
    for at in [0, 3, 60, 61, 200] {
        c[at] = f64::NAN;
    }
    let mut skip = Smma::new(12.0).unwrap().with_skip_nan(true);
    let mut fed = Smma::new(12.0).unwrap();
    let mut plain = Smma::new(12.0).unwrap();
    for (i, &x) in c.iter().enumerate() {
        let want = if x.is_nan() { f64::NAN } else { fed.next_value(x) };
        assert_eq!(bits(skip.next_value(x)), bits(want), "bar {i}");
        assert!(plain.next_value(x).is_nan(), "unset, the NaN at bar 0 poisons every value");
    }
}

// ---------------------------------------------------------------- sma

#[test]
fn sma_fresh_sum_unset_is_the_registry_row() {
    let b = bars();
    for n in [1.0, 20.0, 400.0] {
        let want = registry("sma", &[n], &b);
        let plain = checked_stream("sma", Box::new(Sma::new(n).unwrap()), &b);
        let off = Sma::new(n).unwrap().with_fresh_sum(false);
        assert_lines_same("sma", &plain, &want);
        assert_lines_same("sma, option off", &checked_stream("sma off", Box::new(off), &b), &want);
    }
}

/// `fresh_sum` is `triple_sma_stack_latch`'s average: the window summed afresh, divided by its
/// length. On a high price level the running sum's last bits differ from it on some bars.
#[test]
fn sma_fresh_sum_is_the_window_summed_afresh() {
    for (level, step) in [(100.0, 2.0), (1.0e6, 0.01)] {
        let mut c = closes(level, step, 3000);
        c[1000] = f64::NAN;
        for n in [3usize, 10, 20, 50] {
            let mut fresh = Sma::new(n as f64).unwrap().with_fresh_sum(true);
            let mut plain = Sma::new(n as f64).unwrap();
            let mut window: VecDeque<f64> = VecDeque::new();
            let mut differs = 0usize;
            for (i, &x) in c.iter().enumerate() {
                window.push_back(x);
                if window.len() > n {
                    window.pop_front();
                }
                let want = if window.len() == n {
                    window.iter().sum::<f64>() / n as f64
                } else {
                    f64::NAN
                };
                let got = fresh.next_value(x);
                assert_eq!(bits(got), bits(want), "sma({n}) fresh_sum at level {level}, bar {i}");
                differs += usize::from(bits(plain.next_value(x)) != bits(got));
            }
            if level > 1.0e5 {
                assert!(
                    differs > 0,
                    "sma({n}): the running sum never differed, the test shows nothing"
                );
            }
        }
    }
    let b = bars();
    checked_stream("sma fresh_sum", Box::new(Sma::new(20.0).unwrap().with_fresh_sum(true)), &b);
}

// ---------------------------------------------------------------- vwap

/// Hourly bars, so a UTC day is 24 of them, with a volume that varies.
pub(crate) fn hourly_bars() -> Vec<Bar> {
    let mut r = XorShift(0x5DEE_CE66_D1CE_4E5B);
    closes(100.0, 2.0, 24 * 40 + 7)
        .iter()
        .enumerate()
        .map(|(i, &x)| {
            let (up, down) = (0.1 + r.unit(), 0.1 + r.unit());
            Bar::ohlcv(i as i64 * 3_600_000, x, x + up, x - down, x, 1.0 + 1000.0 * r.unit())
        })
        .collect()
}

/// `vwap_bands_breakout_1sigma`'s `DailyVwapBands` (its VWAP line), fed `source`.
fn daily_vwap(bars: &[Bar], source: impl Fn(&Bar) -> f64) -> Vec<f64> {
    const DAY_MS: i64 = 86_400_000;
    let (mut day, mut volume, mut price_volume) = (i64::MIN, 0.0, 0.0);
    bars.iter()
        .map(|bar| {
            let d = bar.timestamp.div_euclid(DAY_MS);
            if d != day {
                day = d;
                volume = 0.0;
                price_volume = 0.0;
            }
            volume += bar.volume;
            price_volume += source(bar) * bar.volume;
            price_volume / volume
        })
        .collect()
}

#[test]
fn vwap_source_unset_is_the_registry_row() {
    let b = hourly_bars();
    let want = registry("vwap", &[], &b);
    assert_lines_same("vwap", &checked_stream("vwap", Box::new(Vwap::default()), &b), &want);
    let typical = Vwap::default().with_source(VwapSource::Typical);
    assert_lines_same(
        "vwap typical",
        &checked_stream("vwap typical", Box::new(typical), &b),
        &want,
    );
    assert_eq!(VwapSource::default(), VwapSource::Typical);
}

/// `Close` is the line of `vwap_bands_breakout_1sigma`, `TypicalDivided` that of `volume_tide_vwap_mfi`
/// (`(high + low + close) / 3`); the default multiplies by a third and differs in the last bit on
/// some bars.
#[test]
fn vwap_sources_are_the_strategies_daily_vwap() {
    let b = hourly_bars();
    let close =
        checked_stream("vwap close", Box::new(Vwap::default().with_source(VwapSource::Close)), &b);
    assert!(same(&close[0], &daily_vwap(&b, |bar| bar.close)), "vwap Close");
    let divided = checked_stream(
        "vwap typical divided",
        Box::new(Vwap::default().with_source(VwapSource::TypicalDivided)),
        &b,
    );
    let want = daily_vwap(&b, |bar| (bar.high + bar.low + bar.close) / 3.0);
    assert!(same(&divided[0], &want), "vwap TypicalDivided");
    let default = registry("vwap", &[], &b);
    let differ =
        default[0].iter().zip(&divided[0]).filter(|(x, y)| x.to_bits() != y.to_bits()).count();
    assert!(differ > 0, "`* (1/3)` and `/ 3` never differed: the test shows nothing");
}

// ---------------------------------------------------------------- psar

#[test]
fn psar_options_unset_are_the_registry_row() {
    let b = bars();
    for (step, max_af) in [(0.02, 0.2), (0.25, 0.5), (0.001, 1.0)] {
        let want = registry("psar", &[step, max_af], &b);
        let plain = checked_stream("psar", Box::new(Psar::new(step, max_af).unwrap()), &b);
        let off = Psar::new(step, max_af)
            .unwrap()
            .with_start(step)
            .unwrap()
            .with_test_first(false)
            .with_direction_from_closes(false)
            .with_textbook_steps(false);
        assert_lines_same("psar", &plain, &want);
        assert_lines_same(
            "psar, options off",
            &checked_stream("psar off", Box::new(off), &b),
            &want,
        );
    }
}

/// Hand-worked, every number exact: step 0.25, cap 0.5. Bar 3's low (10.5) is under the stop as it
/// moved (11) but above the stop clamped to the last two lows (10): clamped first it does not reverse
/// (10), tested first it does, and the stop is the old extreme 13 (`sar_flip_entry`'s rule).
#[test]
fn psar_test_first_reverses_on_the_unclamped_stop() {
    let hl = [(10.0, 9.0), (12.0, 10.0), (13.0, 12.0), (12.5, 10.5)];
    let run = |mut p: Psar| -> Vec<f64> { hl.iter().map(|&(h, l)| p.next_hl(h, l)).collect() };
    assert_eq!(run(Psar::new(0.25, 0.5).unwrap()), [9.0, 9.0, 9.0, 10.0]);
    assert_eq!(run(Psar::new(0.25, 0.5).unwrap().with_test_first(true)), [9.0, 9.0, 9.0, 13.0]);
    let b = bars();
    let tested = Psar::new(0.02, 0.2).unwrap().with_start(0.018).unwrap().with_test_first(true);
    let got = checked_stream("psar test_first", Box::new(tested), &b);
    assert!(!same(&got[0], &registry("psar", &[0.02, 0.2], &b)[0]), "test_first changed nothing");
}

/// Hand-worked, every number exact: step 0.25, cap 1. By bar 3 the trend has made two new extremes,
/// so its factor is `start + 2 * step`: 0.75 with the start unset (stop 18), 0.625 with a start of
/// 0.125 (stop 16.5); the lows of the last two bars (19) do not clamp either.
#[test]
fn psar_start_is_the_first_extremes_factor() {
    let hl = [(10.0, 9.0), (20.0, 19.0), (21.0, 20.0), (22.0, 21.0)];
    let run = |mut p: Psar| -> Vec<f64> { hl.iter().map(|&(h, l)| p.next_hl(h, l)).collect() };
    assert_eq!(run(Psar::new(0.25, 1.0).unwrap()), [9.0, 9.0, 9.0, 18.0]);
    assert_eq!(
        run(Psar::new(0.25, 1.0).unwrap().with_start(0.125).unwrap()),
        [9.0, 9.0, 9.0, 16.5]
    );
    assert!(Psar::default().with_start(0.0).is_err());
    assert!(Psar::default().with_start(f64::NAN).is_err());
    assert!(Psar::default().with_start(0.6).is_err());
}

/// `sar_flip_entry`'s `ReversalStop` (`pendulum_sar_rotation`'s `TurningStop` and
/// `recompute_alma_sar_corr_magnifier`'s `ParabolicStop` are the same steps), its three constants
/// made parameters: no value on the first bar, the trend started on the second from the two closes,
/// the stop moved as `stop + accel * (extreme - stop)`, the reversal tested before the clamp and the
/// factor carried by repeated adds.
struct ReversalStop {
    start: f64,
    step: f64,
    cap: f64,
    rising: bool,
    stop: f64,
    extreme: f64,
    accel: f64,
    close_1: f64,
    high_1: f64,
    low_1: f64,
    high_2: f64,
    low_2: f64,
}

impl ReversalStop {
    fn new(start: f64, step: f64, cap: f64) -> Self {
        let n = f64::NAN;
        ReversalStop {
            start,
            step,
            cap,
            rising: false,
            stop: n,
            extreme: n,
            accel: start,
            close_1: n,
            high_1: n,
            low_1: n,
            high_2: n,
            low_2: n,
        }
    }

    fn advance(&mut self, bar: &Bar) -> bool {
        let mut new_trend = false;
        if self.stop.is_nan() {
            self.rising = bar.close > self.close_1;
            self.extreme = if self.rising { bar.high } else { bar.low };
            self.stop = if self.rising { self.low_1 } else { self.high_1 };
            new_trend = true;
        }
        self.stop += self.accel * (self.extreme - self.stop);
        let touched = if self.rising { self.stop > bar.low } else { self.stop < bar.high };
        if touched {
            self.stop =
                if self.rising { bar.high.max(self.extreme) } else { bar.low.min(self.extreme) };
            self.rising = !self.rising;
            self.extreme = if self.rising { bar.high } else { bar.low };
            self.accel = self.start;
            new_trend = true;
        }
        new_trend
    }

    fn next(&mut self, bar: &Bar) -> f64 {
        if !self.close_1.is_nan() {
            let new_trend = self.advance(bar);
            let further =
                if self.rising { bar.high > self.extreme } else { bar.low < self.extreme };
            if !new_trend && further {
                self.extreme = if self.rising { bar.high } else { bar.low };
                self.accel = (self.accel + self.step).min(self.cap);
            }
            self.stop = if self.rising {
                self.stop.min(self.low_1).min(self.low_2)
            } else {
                self.stop.max(self.high_1).max(self.high_2)
            };
        }
        (self.high_2, self.low_2) = (self.high_1, self.low_1);
        (self.high_1, self.low_1, self.close_1) = (bar.high, bar.low, bar.close);
        self.stop
    }
}

/// With all four options set, the stop IS the strategies' parabolic stop, bit for bit from the first
/// bar, at `sar_flip_entry`'s constants and at `pendulum_sar_rotation`'s (start 0.018, step 0.021,
/// cap 0.19), over long walks at three price levels and a series with flat stretches, gaps and a
/// bar without range. Every option alone keeps the stream equal to `vectorize` and survives `reset`.
#[test]
fn psar_options_are_the_strategies_parabolic_stop_from_the_first_bar() {
    let mut sets: Vec<(String, Vec<Bar>)> = Vec::new();
    for (level, step) in [(100.0, 2.0), (1.0e6, 0.01), (1.0e-3, 1.0e-6)] {
        sets.push((format!("walk at {level}"), spread_bars(&closes(level, step, 6000))));
    }
    sets.push(("awkward".to_string(), crate::fast::awkward_bars(3000, 400..460, 0..0)));
    for (start, step, cap) in [(0.02, 0.02, 0.2), (0.018, 0.021, 0.19)] {
        for (name, b) in &sets {
            let mut want = ReversalStop::new(start, step, cap);
            let mut got = Psar::new(step, cap)
                .unwrap()
                .with_start(start)
                .unwrap()
                .with_test_first(true)
                .with_direction_from_closes(true)
                .with_textbook_steps(true);
            let mut flips = 0usize;
            let mut was_below = false;
            for (i, bar) in b.iter().enumerate() {
                let w = want.next(bar);
                assert_eq!(
                    bits(got.next(bar)),
                    bits(w),
                    "{name} ({start}, {step}, {cap}), bar {i}"
                );
                // What the bar leaves: the direction, and the stop as the next bar will first move it.
                let (trend, ahead) = if i == 0 {
                    (None, f64::NAN)
                } else {
                    (Some(want.rising), w + want.accel * (want.extreme - w))
                };
                assert_eq!(got.uptrend(), trend, "{name} uptrend, bar {i}");
                assert_eq!(bits(got.stop_ahead()), bits(ahead), "{name} stop_ahead, bar {i}");
                let below = w < bar.close;
                flips += usize::from(i > 1 && below != was_below);
                was_below = below;
            }
            if name == "walk at 100" {
                assert!(flips > 50, "{name}: only {flips} flips, the series tests little");
            }
        }
    }
    let b = bars();
    let options: [fn(Psar) -> Psar; 3] = [
        |p| p.with_test_first(true),
        |p| p.with_direction_from_closes(true),
        |p| p.with_textbook_steps(true),
    ];
    for (k, set) in options.iter().enumerate() {
        let got = checked_stream("psar option", Box::new(set(Psar::default())), &b);
        assert!(
            !same(&got[0], &registry("psar", &[0.02, 0.2], &b)[0]),
            "option {k} changed nothing"
        );
    }
    let mut p = Psar::default().with_direction_from_closes(true);
    assert_eq!(p.lookback(), 1);
    assert!(p.next_hlc(10.0, 9.0, 9.5).is_nan(), "no value on the first bar");
}

// ---------------------------------------------------------------- kama

/// The adaptive average a strategy computes on its own terms (a window not yet full, or flat, reads `flat`), in
/// the crate's recurrence `avg * (1 - sc) + close * sc`; seeded with the first close (`seed_first`) or
/// with the close at `n - 1`, answering from `n`.
fn kama_ref(c: &[f64], n: usize, flat: f64, seed_first: bool) -> Vec<f64> {
    let (fast, slow) = (2.0 / 3.0, 2.0 / 31.0);
    let spread = fast - slow;
    let mut avg = f64::NAN;
    c.iter()
        .enumerate()
        .map(|(i, &x)| {
            if !seed_first && i < n {
                if i + 1 == n {
                    avg = x;
                }
                return f64::NAN;
            }
            let er = if i < n {
                flat
            } else {
                let w = &c[i - n..=i];
                let path = w.windows(2).fold(0.0, |a, p| a + (p[1] - p[0]).abs());
                if path > 0.0 { (w[n] - w[0]).abs() / path } else { flat }
            };
            let s = er * spread + slow;
            let sc = s * s;
            avg = if avg.is_nan() { x } else { avg * (1.0 - sc) + x * sc };
            avg
        })
        .collect()
}

/// Whole-number closes (every path is then exact, so the crate's running path and the window's fresh
/// sum agree), with a flat stretch longer than the window.
fn whole_closes() -> Vec<f64> {
    let mut c: Vec<f64> =
        (0..400u64).map(|i| 100.0 + ((i * 37 + i * i * 11) % 17) as f64).collect();
    c[100..112].fill(105.0);
    c
}

#[test]
fn kama_options_unset_are_the_registry_row() {
    let b = bars();
    for p in [[10.0, 2.0, 30.0], [2.0, 1.0, 400.0]] {
        let want = registry("kama", &p, &b);
        let plain = checked_stream("kama", Box::new(Kama::new(p[0], p[1], p[2]).unwrap()), &b);
        let off = Kama::new(p[0], p[1], p[2])
            .unwrap()
            .with_seed_first(false)
            .with_textbook_steps(false)
            .with_flat_ratio(1.0);
        assert_lines_same("kama", &plain, &want);
        assert_lines_same(
            "kama, options off",
            &checked_stream("kama off", Box::new(off.unwrap()), &b),
            &want,
        );
    }
    let c = whole_closes();
    let mut k = Kama::new(4.0, 2.0, 30.0).unwrap();
    let got: Vec<f64> = c.iter().map(|&x| k.next_value(x)).collect();
    assert!(same(&got, &kama_ref(&c, 4, 1.0, false)), "unset, the reference reads the default too");
}

#[test]
fn kama_seed_first_and_flat_ratio_are_the_average_on_its_own_terms() {
    let c = whole_closes();
    for (seed_first, flat) in [(true, 0.0), (false, 0.0), (true, 1.0)] {
        for n in [4usize, 14] {
            let k = Kama::new(n as f64, 2.0, 30.0).unwrap().with_seed_first(seed_first);
            let mut k = k.with_flat_ratio(flat).unwrap();
            let got: Vec<f64> = c.iter().map(|&x| k.next_value(x)).collect();
            assert!(
                same(&got, &kama_ref(&c, n, flat, seed_first)),
                "kama({n}) seed_first {seed_first} flat_ratio {flat}"
            );
        }
    }
    let k = Kama::default().with_seed_first(true).with_flat_ratio(0.0).unwrap();
    assert_eq!(k.lookback(), 0);
    checked_stream("kama options", Box::new(k), &bars());
    assert!(Kama::default().with_flat_ratio(1.5).is_err());
    assert!(Kama::default().with_flat_ratio(f64::NAN).is_err());
}

/// The textbook adaptive average as a strategy writes it by hand: a ring of `n + 1` closes, the path summed afresh, a window not yet full or flat reading 0,
/// seeded with the first close and moved as `avg + speed * speed * (close - avg)`.
fn textbook_average(c: &[f64], n: usize) -> Vec<f64> {
    const FAST_SPEED: f64 = 2.0 / 3.0;
    const SLOW_SPEED: f64 = 2.0 / 31.0;
    let mut closes = vec![f64::NAN; n + 1];
    let mut average = f64::NAN;
    c.iter()
        .map(|&close| {
            closes.rotate_left(1);
            closes[n] = close;
            let path: f64 = closes.windows(2).map(|pair| (pair[1] - pair[0]).abs()).sum();
            let efficiency = if path > 0.0 { (closes[n] - closes[0]).abs() / path } else { 0.0 };
            let speed = efficiency * (FAST_SPEED - SLOW_SPEED) + SLOW_SPEED;
            average =
                if average.is_nan() { close } else { average + speed * speed * (close - average) };
            average
        })
        .collect()
}

/// With `seed_first`, a flat ratio of 0 and the textbook steps, `kama` IS the textbook average, bit
/// for bit from the first bar, on walks at three price levels with a flat stretch (general numbers,
/// where the running path and the clamp of the default would move the last bits).
#[test]
fn kama_textbook_steps_are_the_textbook_average_bit_for_bit() {
    for (level, step) in [(100.0, 2.0), (1.0e6, 0.01), (1.0e-3, 1.0e-6)] {
        let mut c = closes(level, step, 6000);
        let flat = c[3000];
        c[3000..3040].fill(flat);
        for n in [14usize, 10, 2] {
            let mut k = Kama::new(n as f64, 2.0, 30.0)
                .unwrap()
                .with_seed_first(true)
                .with_textbook_steps(true)
                .with_flat_ratio(0.0)
                .unwrap();
            let got: Vec<f64> = c.iter().map(|&x| k.next_value(x)).collect();
            let want = textbook_average(&c, n);
            for (i, (g, w)) in got.iter().zip(&want).enumerate() {
                assert_eq!(bits(*g), bits(*w), "kama({n}) at level {level}, bar {i}");
            }
        }
    }
    let k = Kama::default().with_textbook_steps(true);
    let got = checked_stream("kama textbook", Box::new(k), &bars());
    assert!(!same(&got[0], &registry("kama", &[], &bars())[0]), "textbook_steps changed nothing");
}

// ---------------------------------------------------------------- adx

#[test]
fn adx_period_unset_is_the_registry_row() {
    let b = bars();
    for n in [2.0, 14.0, 100.0] {
        let want = registry("adx", &[n], &b);
        let plain = checked_stream("adx", Box::new(Adx::new(n).unwrap()), &b);
        let same_period = Adx::new(n).unwrap().with_adx_period(n).unwrap();
        assert_lines_same("adx", &plain, &want);
        assert_lines_same(
            "adx, adx_period = period",
            &checked_stream("adx same", Box::new(same_period), &b),
            &want,
        );
    }
}

/// `meridian_dmi_regime`'s shape: DI over 14, the ADX smoothed over 12 — the mean of the first 12 DX
/// values, then Wilder's smoothing over 12 — while the DI lines are the plain row's.
#[test]
fn adx_period_smooths_the_adx_alone() {
    let b = bars();
    let (n, m) = (14usize, 12usize);
    let mut plain = Adx::new(n as f64).unwrap();
    let mut set = Adx::new(n as f64).unwrap().with_adx_period(m as f64).unwrap();
    assert_eq!(set.lookback_full(), n + m - 1);
    let (inverse, keep) = (1.0 / m as f64, (m as f64 - 1.0) / m as f64);
    let (mut dx_sum, mut adx, mut count) = (-0.0_f64, f64::NAN, 0usize);
    for (i, bar) in b.iter().enumerate() {
        let [_, pdi, mdi] = plain.next(bar);
        let [got, set_pdi, set_mdi] = set.next(bar);
        assert!(same(&[set_pdi, set_mdi], &[pdi, mdi]), "bar {i}: the DI lines moved");
        if pdi.is_nan() {
            assert!(got.is_nan(), "bar {i}");
            continue;
        }
        let denominator = pdi + mdi;
        let dx = if denominator > 0.0 { 100.0 * (pdi - mdi).abs() / denominator } else { 0.0 };
        count += 1;
        let want = if count < m {
            dx_sum += dx;
            f64::NAN
        } else if count == m {
            dx_sum += dx;
            assert_eq!(i, n + m - 1, "the ADX starts at bar period + adx_period - 1");
            adx = dx_sum * inverse;
            adx
        } else {
            adx = adx * keep + dx * inverse;
            adx
        };
        assert_eq!(bits(got), bits(want), "bar {i}");
    }
    let set = Adx::new(14.0).unwrap().with_adx_period(12.0).unwrap();
    checked_stream("adx adx_period", Box::new(set), &b);
    assert!(Adx::default().with_adx_period(1.0).is_err());
    assert!(Adx::default().with_adx_period(f64::NAN).is_err());
}

// ---------------------------------------------------------------- keltner

#[test]
fn keltner_range_ema_unset_is_the_registry_row() {
    let b = bars();
    for p in [[20.0, 10.0, 2.0], [24.0, 24.0, 1.7], [1.0, 1.0, 0.1]] {
        let want = registry("keltner", &p, &b);
        let plain =
            checked_stream("keltner", Box::new(Keltner::new(p[0], p[1], p[2]).unwrap()), &b);
        let off = Keltner::new(p[0], p[1], p[2]).unwrap().with_range_ema(false);
        assert_lines_same("keltner", &plain, &want);
        assert_lines_same(
            "keltner, option off",
            &checked_stream("keltner off", Box::new(off), &b),
            &want,
        );
    }
}

/// `mantle_keltner_regime`'s channel, built from the crate's own `Ema` and `TrueRange` the way that
/// strategy builds it: an EMA of the close, and `mult` EMAs of the true range, the first bar (which
/// has no previous close) not fed to the range average.
#[test]
fn keltner_range_ema_is_mantle_keltner_regimes_channel() {
    let b = bars();
    for (ema_n, atr_n, mult) in [(24.0, 24.0, 1.7), (20.0, 20.0, 1.5), (20.0, 10.0, 2.0)] {
        let mut k = Keltner::new(ema_n, atr_n, mult).unwrap().with_range_ema(true);
        let mut centre = Ema::new(ema_n).unwrap();
        let mut true_range = TrueRange::default();
        let mut range_average = Ema::new(atr_n).unwrap();
        for (i, bar) in b.iter().enumerate() {
            let mid = centre.next_value(bar.close);
            let range = true_range.next(bar);
            let band = if i > 0 { range_average.next_value(range) * mult } else { f64::NAN };
            let got = k.next(bar);
            assert!(
                same(&got, &[mid + band, mid, mid - band]),
                "keltner({ema_n}, {atr_n}, {mult}) range_ema, bar {i}"
            );
        }
    }
    let k = Keltner::new(20.0, 10.0, 2.0).unwrap().with_range_ema(true);
    assert_eq!(k.lookback_full(), 19);
    let k = Keltner::new(10.0, 20.0, 2.0).unwrap().with_range_ema(true);
    assert_eq!(k.lookback_full(), 20);
    checked_stream("keltner range_ema", Box::new(k), &b);
}

// ---------------------------------------------------------------- rank_correlation

/// `zenith_rci_wpr`'s `time_rank_rho`: ties share the mean of the ranks they span.
fn time_rank_rho(window: &VecDeque<f64>) -> f64 {
    let n = window.len() as f64;
    let squares: f64 = window
        .iter()
        .zip(1_i32..)
        .map(|(close, place)| {
            let lower = window.iter().filter(|c| *c < close).count() as f64;
            let equal = window.iter().filter(|c| *c == close).count() as f64;
            let gap = f64::from(place) - (lower + (equal + 1.0) / 2.0);
            gap * gap
        })
        .sum();
    100.0 * (1.0 - 6.0 * squares / (n * (n * n - 1.0)))
}

#[test]
fn rank_correlation_mean_ties_unset_is_the_registry_row() {
    let b = bars();
    for n in [2.0, 14.0, 100.0] {
        let want = registry("rank_correlation", &[n], &b);
        let plain = checked_stream("rci", Box::new(RankCorrelation::new(n).unwrap()), &b);
        let off = RankCorrelation::new(n).unwrap().with_mean_rank_ties(false);
        assert_lines_same("rci", &plain, &want);
        assert_lines_same("rci, option off", &checked_stream("rci off", Box::new(off), &b), &want);
    }
}

/// Closes on a grid of six values, so most windows hold ties: the option is `time_rank_rho` bit for
/// bit, and differs from the age-ranked default. Without a tie the two agree.
#[test]
fn rank_correlation_mean_ties_is_zenith_rci_wprs() {
    let mut r = XorShift(0x2545_F491_4F6C_DD1D);
    let c: Vec<f64> = (0..1200).map(|_| 100.0 + (r.unit() * 6.0).floor()).collect();
    for n in [14usize, 17, 2] {
        let mut mean = RankCorrelation::new(n as f64).unwrap().with_mean_rank_ties(true);
        let mut aged = RankCorrelation::new(n as f64).unwrap();
        let mut window: VecDeque<f64> = VecDeque::new();
        let mut differs = 0usize;
        for (i, &x) in c.iter().enumerate() {
            window.push_back(x);
            if window.len() > n {
                window.pop_front();
            }
            let want = if window.len() == n { time_rank_rho(&window) } else { f64::NAN };
            let got = mean.next_value(x);
            assert_eq!(bits(got), bits(want), "rci({n}) mean ties, bar {i}");
            differs += usize::from(bits(aged.next_value(x)) != bits(got));
        }
        assert!(differs > 0, "rci({n}): ties never changed the answer, the test shows nothing");
    }
    let walk = closes(100.0, 2.0, 1000);
    let mut mean = RankCorrelation::new(14.0).unwrap().with_mean_rank_ties(true);
    let mut aged = RankCorrelation::new(14.0).unwrap();
    for (i, &x) in walk.iter().enumerate() {
        assert_eq!(bits(mean.next_value(x)), bits(aged.next_value(x)), "no tie, bar {i}");
    }
    let set = RankCorrelation::new(17.0).unwrap().with_mean_rank_ties(true);
    checked_stream("rci mean ties", Box::new(set), &bars());
}
