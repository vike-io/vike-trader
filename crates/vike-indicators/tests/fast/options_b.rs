//! The typed options and readers added after `options.rs` filled: `psar`'s reversal on the close, its
//! ATR step, its extreme-first law and the state a bar leaves; `hma`'s rounded root, rounded half
//! and unit lengths; `heikin_ashi`'s tick; `confirmed_pivot`'s `right` of 0; `vwap`'s first bar and
//! deviation; `donchian`'s extreme offsets and restart at a missing value; `rsi`'s RS form,
//! saturation and zero saturation, `stochrsi`'s RS form and `pivot_points`' Fibonacci levels;
//! `atr`'s first range seed; `smma`'s alpha steps; `adx`'s zero-seeded sums, first-bar moves, held
//! lines, missing ratios, first-bar range and DX from the first bar, `supertrend`'s previous bands
//! and down start and `chandelier`'s close extremes and ratchet; and the `swma` row. Then (I5)
//! `smma`'s, `rsi`'s, `ema`'s, `kama`'s and `vidya`'s zero seeds, `t3`'s six EMAs,
//! `atr_trailing_stop`'s down start, `true_range`'s missing first bar, `chop`'s true extremes,
//! `confirmed_pivot`'s spans to 200, `stochastic`'s fresh sums, `aroon`'s older ties, `zscore`'s
//! sample deviation, and `supertrend`'s gated turns, carry on close, held direction and
//! `next_centred`. The same contracts as there: off by default
//! the registry row's numbers, set the definition its doc names bit for bit, and the stream equal
//! to `vectorize` and kept by `reset`.

use vike_indicators::indicators::{Atr, Donchian, Hma, Psar, Swma, Vwap, VwapSource, Wma};
use vike_indicators::{Indicator, series};
use vike_marketdata::Bar;

use crate::fast::{assert_close, awkward_bars, closes, same};
use crate::options::{assert_lines_same, bits, checked_stream, hourly_bars, registry};

/// Bars with a body: each opens at the last close, so a close above its open means something.
fn bodied_bars(c: &[f64]) -> Vec<Bar> {
    let mut open = c[0] - 0.5;
    c.iter()
        .enumerate()
        .map(|(i, &x)| {
            let (o, up, down) = (open, 0.3 + (i % 7) as f64 * 0.1, 0.2 + (i % 5) as f64 * 0.15);
            open = x;
            Bar::ohlcv(i as i64 * 60_000, o, o.max(x) + up, o.min(x) - down, x, 1.0)
        })
        .collect()
}

// ---------------------------------------------------------------- psar

/// QuantNomad's SAR judged on the close as `parabolic_sar_strategy_on_close_quantnomad` wrote it,
/// and with `atr` as `atr_parabolic_sar_strategy_quantnomad` did, their constants made parameters.
struct CloseSar {
    start: f64,
    step: f64,
    cap: f64,
    atr: Option<Atr>,
    bars: usize,
    previous: Option<Bar>,
    psar: f64,
    af: f64,
    dir: i32,
    ep: f64,
}

impl CloseSar {
    fn new(start: f64, step: f64, cap: f64, atr: Option<f64>) -> Self {
        let atr = atr.map(|n| Atr::new(n).unwrap());
        let n = f64::NAN;
        CloseSar { start, step, cap, atr, bars: 0, previous: None, psar: n, af: n, dir: 0, ep: n }
    }

    fn next(&mut self, bar: &Bar) -> f64 {
        let range = self.previous.as_ref().map_or(f64::NAN, |p| {
            (bar.high - bar.low).max((bar.high - p.close).abs()).max((bar.low - p.close).abs())
        });
        let atr =
            self.atr.as_mut().map(|a| a.next(bar)).map(|a| if a.is_nan() { range } else { a });
        let previous = self.previous.replace(bar.clone());
        self.bars += 1;
        let Some(previous) = previous else { return f64::NAN };
        let first = self.bars == 2;
        let to_short = self.dir == 1 && bar.close <= self.psar;
        let to_long = self.dir == -1 && bar.close >= self.psar;
        let change = first || to_short || to_long;
        self.dir = if first {
            if previous.close > previous.open { 1 } else { -1 }
        } else if to_short {
            -1
        } else if to_long {
            1
        } else {
            self.dir
        };
        let up = self.dir == 1;
        let new_extreme = (up && bar.high > self.ep) || (!up && bar.low < self.ep);
        self.af = if change {
            self.start
        } else if new_extreme {
            self.cap.min(self.af + self.step)
        } else {
            self.af
        };
        let last_ep = self.ep;
        self.ep = if change {
            if up { bar.high } else { bar.low }
        } else if up {
            last_ep.max(bar.high)
        } else {
            last_ep.min(bar.low)
        };
        self.psar = if first {
            if up { previous.low } else { previous.high }
        } else if change {
            last_ep
        } else {
            match atr {
                Some(d) if up => self.psar + self.af * d,
                Some(d) => self.psar - self.af * d,
                None if up => self.psar + self.af * (self.ep - self.psar),
                None => self.psar - self.af * (self.psar - self.ep),
            }
        };
        self.psar
    }
}

/// Set, `reversal_on_close` IS QuantNomad's SAR (with `atr_step`, his ATR one), bit for bit from the
/// first bar, its direction and its value as the level ahead included, over long walks at three
/// price levels and a series with flat stretches, gaps and a bar without range (a cap under 0.05
/// among the factors); the stream equals `vectorize` and survives `reset`.
#[test]
fn psar_reversal_on_close_is_quantnomads_sar() {
    let mut sets: Vec<(String, Vec<Bar>)> = Vec::new();
    for (level, step) in [(100.0, 2.0), (1.0e6, 0.01), (1.0e-3, 1.0e-6)] {
        sets.push((format!("walk at {level}"), bodied_bars(&closes(level, step, 6000))));
    }
    sets.push(("awkward".to_string(), awkward_bars(3000, 400..460, 0..0)));
    let factors =
        [(0.02, 0.02, 0.2, None), (0.03, 0.02, 0.025, None), (0.02, 0.02, 0.2, Some(14.0))];
    for (start, step, cap, atr) in factors {
        for (name, b) in &sets {
            let tag = format!("{name} ({start}, {step}, {cap}, atr {atr:?})");
            let build = || {
                let p = Psar::new(step, cap).unwrap().with_start(start).unwrap();
                let p = p.with_reversal_on_close(true);
                atr.map_or(p.clone(), |n| p.with_atr_step(n).unwrap())
            };
            checked_stream(&tag, Box::new(build()), b);
            let mut want = CloseSar::new(start, step, cap, atr);
            let mut got = build();
            let mut flips = 0usize;
            for (i, bar) in b.iter().enumerate() {
                let was = want.dir;
                let w = want.next(bar);
                assert_eq!(bits(got.next(bar)), bits(w), "{tag}, bar {i}");
                let trend = (want.dir != 0).then_some(want.dir == 1);
                assert_eq!(got.uptrend(), trend, "{tag} uptrend, bar {i}");
                assert_eq!(bits(got.stop_ahead()), bits(w), "{tag} stop_ahead, bar {i}");
                flips += usize::from(was != 0 && was != want.dir);
            }
            if name == "walk at 100" {
                assert!(flips > 50, "{tag}: only {flips} flips, the series tests little");
            }
        }
    }
    assert!(Psar::default().with_atr_step(0.0).is_err());
    assert!(Psar::default().with_atr_step(f64::NAN).is_err());
    let b = bodied_bars(&closes(100.0, 2.0, 1500));
    let on_close =
        checked_stream("on close", Box::new(Psar::default().with_reversal_on_close(true)), &b);
    assert!(
        !same(&on_close[0], &registry("psar", &[0.02, 0.2], &b)[0]),
        "on close changed nothing"
    );
    assert_eq!(Psar::default().with_reversal_on_close(true).lookback(), 1);
}

/// Hand-worked, every number exact: step 0.25, cap 0.5. The first bar closes above its open, so the
/// second starts an up-trend at the first low (9) with the second high (12) its extreme; the third
/// makes a new extreme (13, factor 0.5) and moves the stop half way (11); the fourth closes under 11
/// and reverses at the old extreme (13), its low (10.5) the new extreme; the fifth makes a new low
/// (9.5, factor 0.5) and moves the stop half way down (11.25).
#[test]
fn psar_reversal_on_close_by_hand() {
    let ohlc = [
        (10.0, 11.0, 9.0, 10.5),
        (10.5, 12.0, 10.0, 11.5),
        (11.5, 13.0, 11.0, 12.5),
        (12.5, 12.6, 10.5, 10.8),
        (10.8, 11.0, 9.5, 10.0),
    ];
    let mut p = Psar::new(0.25, 0.5).unwrap().with_reversal_on_close(true);
    let mut got = Vec::new();
    for (i, &(o, h, l, c)) in ohlc.iter().enumerate() {
        got.push((p.next(&Bar::ohlcv(i as i64, o, h, l, c, 1.0)), p.uptrend()));
    }
    let want = [
        (f64::NAN, None),
        (9.0, Some(true)),
        (11.0, Some(true)),
        (13.0, Some(false)),
        (11.25, Some(false)),
    ];
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        assert_eq!((bits(g.0), g.1), (bits(w.0), w.1), "bar {i}");
    }
}

/// The registry row's state as a bar leaves it, by hand (step 0.25, cap 1): no trend before a bar;
/// after the first, an up-trend whose stop moves to `9 * 0.75 + 10 * 0.25 = 9.25` (the next bar
/// clamps it to 9); after the third, `18`, which the fourth bar's lows do not clamp, so it is that
/// bar's value.
#[test]
fn psar_uptrend_and_stop_ahead_by_hand() {
    let hl = [(10.0, 9.0), (20.0, 19.0), (21.0, 20.0), (22.0, 21.0)];
    let mut p = Psar::new(0.25, 1.0).unwrap();
    assert_eq!((p.uptrend(), bits(p.stop_ahead())), (None, bits(f64::NAN)));
    let mut ahead = Vec::new();
    for &(h, l) in &hl {
        p.next_hl(h, l);
        assert_eq!(p.uptrend(), Some(true));
        ahead.push(p.stop_ahead());
    }
    assert_eq!(ahead[0], 9.25);
    assert_eq!(ahead[2], 18.0);
}

// ---------------------------------------------------------------- hma

/// The level-3 strategies' Hull: `Wma(root)` of `2 * Wma(n / 2) - Wma(n)`, the outer average fed
/// from the inner difference's first number on.
fn three_wma_hull(c: &[f64], n: usize, root: usize) -> Vec<f64> {
    hull_of(c, n, n / 2, root)
}

/// [`three_wma_hull`] with the half length given.
fn hull_of(c: &[f64], n: usize, half: usize, root: usize) -> Vec<f64> {
    let wma = |k: usize| Wma::new(k as f64).unwrap();
    let (mut half, mut full, mut outer) = (wma(half), wma(n), wma(root));
    c.iter()
        .map(|&x| {
            let d = 2.0 * half.next_value(x) - full.next_value(x);
            if d.is_nan() { f64::NAN } else { outer.next_value(d) }
        })
        .collect()
}

#[test]
fn hma_rounded_root_unset_is_the_registry_row() {
    let b = crate::options::hourly_bars();
    for n in [2.0, 15.0, 20.0, 480.0, 1000.0] {
        let want = registry("hma", &[n], &b);
        let plain = checked_stream("hma", Box::new(Hma::new(n).unwrap()), &b);
        let off = Hma::new(n).unwrap().with_rounded_root(false);
        assert_lines_same("hma", &plain, &want);
        assert_lines_same("hma off", &checked_stream("hma off", Box::new(off), &b), &want);
    }
    assert!(Hma::new(1000.0).is_ok() && Hma::new(1001.0).is_err());
}

/// Set, the last average's length is `round(sqrt(n))`: the level-3 Hulls (13 -> 4, 15 -> 4,
/// 24 -> 5, 60 -> 8, 480 -> 22) to the running sums' rounding noise, `NaN` exactly where they are
/// and the first value at `lookback`; where rounding and flooring agree (16, 20) not a bit moves.
#[test]
fn hma_rounded_root_is_the_level_three_hulls() {
    let c = closes(100.0, 2.0, 3000);
    let b: Vec<Bar> =
        c.iter().enumerate().map(|(i, &x)| Bar::ohlcv(i as i64, x, x, x, x, 1.0)).collect();
    for (n, root) in [(13usize, 4usize), (15, 4), (24, 5), (60, 8), (480, 22)] {
        let hma = Hma::new(n as f64).unwrap().with_rounded_root(true);
        let lookback = hma.lookback();
        let got = checked_stream("hma rounded", Box::new(hma), &b).remove(0);
        assert_close(
            &format!("hma({n}) rounded"),
            &got,
            &three_wma_hull(&c, n, root),
            1e-9,
            1e-300,
        );
        assert_eq!(got.iter().position(|v| !v.is_nan()), Some(lookback), "hma({n}) lookback");
        let floored = registry("hma", &[n as f64], &b).remove(0);
        assert!(!same(&got, &floored), "hma({n}): rounding changed nothing");
    }
    for n in [16.0, 20.0] {
        let got = checked_stream("hma", Box::new(Hma::new(n).unwrap().with_rounded_root(true)), &b);
        assert_lines_same("hma rounded = floored", &got, &registry("hma", &[n], &b));
    }
}

// ---------------------------------------------------------------- hma half and unit lengths

/// Unset, `rounded_half` and `unit_lengths` are the registry row; the first value is at
/// `lookback` for every period, 2 and 3 included (their root is raised to 2 unset).
#[test]
fn hma_rounded_half_and_unit_lengths_unset_are_the_registry_row() {
    let b = crate::options::hourly_bars();
    for n in [2.0, 3.0, 15.0, 25.0, 400.0] {
        let want = registry("hma", &[n], &b);
        let off = Hma::new(n).unwrap().with_rounded_half(false).with_unit_lengths(false);
        let lookback = off.lookback();
        let got = checked_stream("hma off", Box::new(off), &b);
        assert_lines_same("hma half/unit off", &got, &want);
        assert_eq!(got[0].iter().position(|v| !v.is_nan()), Some(lookback), "hma({n}) lookback");
    }
}

/// `ut_bot_v5`'s HMA(2) of the open (level 3, batch 9): half and root 1, so
/// `2 * x - (2 * x + x[1]) / 3`, to the running sums' rounding noise; HMA(3) is the three
/// averages of 1, 3 and 1. Every period from 4 on has a half and a root of at least 2: not a bit
/// moves there.
#[test]
fn hma_unit_lengths_is_ut_bot_v5s_hull_of_two() {
    let c = closes(1.0e5, 50.0, 6000);
    let b: Vec<Bar> =
        c.iter().enumerate().map(|(i, &x)| Bar::ohlcv(i as i64, x, x, x, x, 1.0)).collect();
    let hma = Hma::new(2.0).unwrap().with_unit_lengths(true);
    assert_eq!(hma.lookback(), 1);
    let got = checked_stream("hma(2) unit", Box::new(hma), &b).remove(0);
    let mut previous = f64::NAN;
    let want: Vec<f64> = c
        .iter()
        .map(|&x| {
            let v = 2.0 * x - (2.0 * x + previous) / 3.0;
            previous = x;
            v
        })
        .collect();
    assert_close("hma(2) unit", &got, &want, 1e-9, 1e-300);
    let floored = registry("hma", &[2.0], &b).remove(0);
    assert!(!same(&got, &floored), "hma(2): unit lengths changed nothing");
    let three = Hma::new(3.0).unwrap().with_unit_lengths(true);
    assert_eq!(three.lookback(), 2);
    let got = checked_stream("hma(3) unit", Box::new(three), &b).remove(0);
    assert_close("hma(3) unit", &got, &hull_of(&c, 3, 1, 1), 1e-9, 1e-300);
    for n in [4.0, 15.0, 25.0] {
        let got = checked_stream("hma", Box::new(Hma::new(n).unwrap().with_unit_lengths(true)), &b);
        assert_lines_same("hma unit = registry", &got, &registry("hma", &[n], &b));
    }
}

/// `tsi_cci_hull_uqfaeu`'s first Hull of the open (level 3, batch 11): the half `round(25 / 2)`
/// = 13, `wma(2 * wma(x, 13) - wma(x, 25), 5)`, to the running sums' rounding noise, `NaN` exactly
/// where it is and the first value at `lookback`; with the root rounded too, 15 is 8, 15, 4. An even
/// period's half is the same either way: not a bit moves.
#[test]
fn hma_rounded_half_is_tsi_cci_hulls_first_average() {
    let c = closes(100.0, 2.0, 3000);
    let b: Vec<Bar> =
        c.iter().enumerate().map(|(i, &x)| Bar::ohlcv(i as i64, x, x, x, x, 1.0)).collect();
    for (n, half, root, rounded_root) in [(25usize, 13usize, 5usize, false), (15, 8, 4, true)] {
        let hma =
            Hma::new(n as f64).unwrap().with_rounded_half(true).with_rounded_root(rounded_root);
        let lookback = hma.lookback();
        let got = checked_stream("hma half", Box::new(hma), &b).remove(0);
        assert_close(&format!("hma({n}) half"), &got, &hull_of(&c, n, half, root), 1e-9, 1e-300);
        assert_eq!(got.iter().position(|v| !v.is_nan()), Some(lookback), "hma({n}) lookback");
        let floored = registry("hma", &[n as f64], &b).remove(0);
        assert!(!same(&got, &floored), "hma({n}): the rounded half changed nothing");
    }
    for n in [2.0, 20.0, 24.0] {
        let got = checked_stream("hma", Box::new(Hma::new(n).unwrap().with_rounded_half(true)), &b);
        assert_lines_same("hma even half", &got, &registry("hma", &[n], &b));
    }
}

// ---------------------------------------------------------------- heikin_ashi tick

use vike_indicators::indicators::{ConfirmedPivot, HeikinAshi};

/// `strategy_lab_b`'s Heikin-Ashi ticker (`timeframe.rs`'s `HeikinAshi`, rule 5): the close and
/// the open each put on the tick as they are made, `round(x * k) / k` with `k = round(1 / tick)`,
/// the next open the mean of the rounded pair; the high and low the bar's widened to both.
/// `generic` instead rounds as `round(x / tick) * tick`, for a tick that is not `1 / k`.
fn ticker(bars: &[Bar], tick: f64, generic: bool) -> Vec<Vec<f64>> {
    let per_tick = (1.0 / tick).round();
    let grid = |x: f64| {
        if generic { (x / tick).round() * tick } else { (x * per_tick).round() / per_tick }
    };
    let mut last: Option<(f64, f64)> = None;
    let mut out = vec![Vec::new(), Vec::new(), Vec::new(), Vec::new()];
    for bar in bars {
        let close = grid((bar.open + bar.high + bar.low + bar.close) / 4.0);
        let open = grid(match last {
            None => (bar.open + bar.close) / 2.0,
            Some((open, close)) => (open + close) / 2.0,
        });
        last = Some((open, close));
        for (line, v) in out.iter_mut().zip([
            open,
            bar.high.max(open.max(close)),
            bar.low.min(open.min(close)),
            close,
        ]) {
            line.push(v);
        }
    }
    out
}

#[test]
fn heikin_ashi_with_tick_is_the_labs_ticker() {
    let mut sets: Vec<(f64, Vec<Bar>)> = Vec::new();
    for (tick, level, step) in [(0.01, 1.0e5, 40.0), (0.00001, 0.2, 0.002), (0.5, 100.0, 2.0)] {
        sets.push((tick, bodied_bars(&closes(level, step, 4000))));
    }
    for (tick, b) in &sets {
        let ha = HeikinAshi::default().with_tick(*tick).unwrap();
        let got = checked_stream(&format!("heikin_ashi tick {tick}"), Box::new(ha), b);
        assert_lines_same(&format!("heikin_ashi tick {tick}"), &got, &ticker(b, *tick, false));
        // Rounding the plain candles is not this recursion: the opens part somewhere.
        let plain = registry("heikin_ashi", &[], b);
        let rounded: Vec<f64> = plain[0]
            .iter()
            .map(|&x| (x * (1.0 / tick).round()).round() / (1.0 / tick).round())
            .collect();
        assert!(
            !same(&got[0], &rounded),
            "tick {tick}: the open's recursion is on the rounded prices"
        );
    }
    // A tick that is not `1 / k`: the generic rounding, never a division by zero.
    for tick in [5.0, 0.3] {
        let b = bodied_bars(&closes(1000.0, 20.0, 2000));
        let got = checked_stream(
            "heikin_ashi coarse",
            Box::new(HeikinAshi::default().with_tick(tick).unwrap()),
            &b,
        );
        assert_lines_same(&format!("heikin_ashi tick {tick}"), &got, &ticker(&b, tick, true));
    }
    let b = crate::options::hourly_bars();
    assert_lines_same(
        "heikin_ashi unset",
        &checked_stream("heikin_ashi", Box::new(HeikinAshi::default()), &b),
        &registry("heikin_ashi", &[], &b),
    );
    for bad in [0.0, -0.01, f64::NAN, f64::INFINITY, 1.0e-320] {
        let err = HeikinAshi::default().with_tick(bad).err().expect("refused");
        assert_eq!((err.indicator, err.param), ("heikin_ashi", "tick"), "tick {bad}");
    }
}

// ---------------------------------------------------------------- confirmed_pivot right 0

/// `ultimate_bullish_divergence_for_rsi_mfi_rvsi_obv`'s `pivot` (level 3, batch 4): the newest
/// value is a pivot low at `left` when the `left + 1` values there are all numbers and none of the
/// `left` before it is lower (a tie with an older value holds); a pivot high mirrors it.
fn own_bar_pivot(x: &[f64], left: usize, low: bool) -> Vec<f64> {
    (0..x.len())
        .map(|i| {
            if i < left {
                return f64::NAN;
            }
            let window = &x[i - left..=i];
            let now = x[i];
            let holds = window.iter().all(|v| !v.is_nan())
                && window[..left].iter().all(|&v| if low { v >= now } else { v <= now });
            if holds { now } else { f64::NAN }
        })
        .collect()
}

/// With `right` 0 a pivot confirms on its own bar: the lab's law bit for bit at its strengths, on
/// whole-number values (ties common) whose missing ones are a warm-up prefix, as the oscillator's
/// are; `last_low` holds it. A missing value inside the window loses here and confirms nothing
/// there, which a prefix never shows.
#[test]
fn confirmed_pivot_right_zero_is_the_labs_own_bar_pivot() {
    let mut x: Vec<f64> = closes(50.0, 3.0, 3000).iter().map(|v| v.round()).collect();
    x[..14].fill(f64::NAN);
    let b: Vec<Bar> =
        x.iter().enumerate().map(|(i, &v)| Bar::ohlcv(i as i64, v, v, v, v, 1.0)).collect();
    for left in [1usize, 2, 13, 54, 100] {
        let p = ConfirmedPivot::new(left as f64, 0.0).unwrap();
        assert_eq!(p.lookback(), left);
        let got = checked_stream(&format!("confirmed_pivot({left}, 0)"), Box::new(p), &b);
        let (highs, lows) = (own_bar_pivot(&x, left, false), own_bar_pivot(&x, left, true));
        assert!(same(&got[0], &highs), "confirmed_pivot({left}, 0): highs");
        assert!(same(&got[1], &lows), "confirmed_pivot({left}, 0): lows");
        let pivots = lows.iter().filter(|v| !v.is_nan()).count();
        assert!(pivots > 10, "confirmed_pivot({left}, 0): only {pivots} pivot lows");
        let mut held = f64::NAN;
        for (i, &l) in lows.iter().enumerate() {
            held = if l.is_nan() { held } else { l };
            assert_eq!(bits(got[3][i]), bits(held), "confirmed_pivot({left}, 0) last_low, bar {i}");
        }
        let slice: Vec<Vec<f64>> =
            series::confirmed_pivot(&x, &x, left as f64, 0.0).unwrap().into();
        assert_lines_same("confirmed_pivot right 0 series", &slice, &got);
    }
    assert_lines_same(
        "confirmed_pivot (5, 0) registry",
        &registry("confirmed_pivot", &[5.0, 0.0], &b),
        &ConfirmedPivot::new(5.0, 0.0).unwrap().vectorize(&b),
    );
    assert!(ConfirmedPivot::new(5.0, -1.0).is_err());
}

// ---------------------------------------------------------------- vwap

/// The day's sums of `vwap_bands_breakout_1sigma`'s `DailyVwapBands`, fed `source`: the VWAP and
/// `sqrt(max(0, sum(v p^2) / sum(v) - vwap^2))` per bar.
fn daily_vwap_deviation(bars: &[Bar], source: impl Fn(&Bar) -> f64) -> (Vec<f64>, Vec<f64>) {
    const DAY_MS: i64 = 86_400_000;
    let (mut day, mut v, mut pv, mut ppv) = (i64::MIN, 0.0, 0.0, 0.0);
    bars.iter()
        .map(|bar| {
            if bar.timestamp.div_euclid(DAY_MS) != day {
                (day, v, pv, ppv) = (bar.timestamp.div_euclid(DAY_MS), 0.0, 0.0, 0.0);
            }
            let p = source(bar);
            v += bar.volume;
            pv += p * bar.volume;
            ppv += bar.volume * (p * p);
            let vwap = pv / v;
            (vwap, (ppv / v - vwap * vwap).max(0.0).sqrt())
        })
        .unzip()
}

#[test]
fn vwap_deviation_is_the_days_volume_weighted_deviation() {
    let b = hourly_bars();
    for source in [VwapSource::Close, VwapSource::TypicalDivided] {
        let price = |bar: &Bar| match source {
            VwapSource::Close => bar.close,
            _ => (bar.high + bar.low + bar.close) / 3.0,
        };
        let (want_vwap, want_dev) = daily_vwap_deviation(&b, price);
        let mut v = Vwap::default().with_source(source);
        assert!(v.deviation().is_nan(), "no deviation before a bar");
        for (i, bar) in b.iter().enumerate() {
            assert_eq!(bits(v.next(bar)), bits(want_vwap[i]), "{source:?} vwap, bar {i}");
            assert_eq!(bits(v.deviation()), bits(want_dev[i]), "{source:?} deviation, bar {i}");
        }
    }
    let mut v = Vwap::default();
    v.next(&Bar::ohlcv(0, 1.0, 1.0, 1.0, 1.0, 0.0));
    assert!(v.deviation().is_nan(), "no volume, no deviation");
}

/// Set, a day's first bar is its price exactly (`0.1 * 3 / 3` is an ulp above 0.1), and every
/// other bar is the registry row's.
#[test]
fn vwap_first_bar_price_is_the_price_exactly() {
    let day = 86_400_000;
    let b = [
        Bar::ohlcv(0, 0.1, 0.1, 0.1, 0.1, 3.0),
        Bar::ohlcv(60_000, 0.2, 0.2, 0.2, 0.2, 5.0),
        Bar::ohlcv(day, 0.1, 0.1, 0.1, 0.1, 3.0),
    ];
    let unset =
        checked_stream("vwap close", Box::new(Vwap::default().with_source(VwapSource::Close)), &b);
    let set = Vwap::default().with_source(VwapSource::Close).with_first_bar_price(true);
    let set = checked_stream("vwap first", Box::new(set), &b);
    assert_ne!(
        unset[0][0], 0.1,
        "the unset first bar must land off the price, or this shows nothing"
    );
    assert_eq!((set[0][0], set[0][2]), (0.1, 0.1));
    assert_eq!(bits(set[0][1]), bits(unset[0][1]));
    let h = hourly_bars();
    let set = Vwap::default().with_first_bar_price(true);
    let set = checked_stream("vwap typical first", Box::new(set), &h).remove(0);
    let unset = registry("vwap", &[], &h).remove(0);
    for (i, (s, u)) in set.iter().zip(&unset).enumerate() {
        let first = i % 24 == 0;
        let typical = (h[i].high + h[i].low + h[i].close) * (1.0 / 3.0);
        assert_eq!(bits(*s), bits(if first { typical } else { *u }), "bar {i}");
    }
}

// ---------------------------------------------------------------- donchian

/// `wolfe_strategy_trendoscope`'s scan: oldest first, strictly beyond, so a tie keeps the older bar.
fn offsets_ref(window: &[(f64, f64)]) -> [usize; 2] {
    let (mut hi, mut lo) = (0, 0);
    for (i, &(h, l)) in window.iter().enumerate() {
        if h > window[hi].0 {
            hi = i;
        }
        if l < window[lo].1 {
            lo = i;
        }
    }
    [window.len() - 1 - hi, window.len() - 1 - lo]
}

#[test]
fn donchian_extreme_offsets_tie_to_the_older_bar() {
    // Whole-number highs and lows tie often.
    let b: Vec<Bar> = closes(100.0, 3.0, 2000)
        .iter()
        .enumerate()
        .map(|(i, &x)| Bar::ohlcv(i as i64, x, (x + 1.0).round(), (x - 1.0).round(), x, 1.0))
        .collect();
    for n in [1.0, 8.0, 9.0, 30.0] {
        let k = n as usize;
        let d = Donchian::new(n).unwrap().with_extreme_offsets(true);
        assert_lines_same(
            "donchian",
            &checked_stream("donchian", Box::new(d.clone()), &b),
            &registry("donchian", &[n], &b),
        );
        let mut d = d;
        let mut ties = 0usize;
        for (i, bar) in b.iter().enumerate() {
            d.next(bar);
            let window: Vec<(f64, f64)> =
                b[(i + 1).saturating_sub(k)..=i].iter().map(|x| (x.high, x.low)).collect();
            let want = (window.len() == k).then(|| offsets_ref(&window));
            assert_eq!(d.extreme_offsets(), want, "donchian({n}) bar {i}");
            ties += usize::from(window.iter().filter(|w| w.0 == window[0].0).count() > 1);
        }
        assert!(k == 1 || ties > 100, "donchian({n}): only {ties} ties");
    }
    let mut d = Donchian::new(4.0).unwrap().with_extreme_offsets(true);
    for (h, l) in [(5.0, 3.0), (7.0, 2.0), (7.0, 4.0), (6.0, 2.0)] {
        d.next_hl(h, l);
    }
    assert_eq!(d.extreme_offsets(), Some([2, 2]), "the older 7 and the older 2");
    let mut plain = Donchian::new(4.0).unwrap();
    plain.next_hl(1.0, 0.0);
    assert_eq!(plain.extreme_offsets(), None, "unset, nothing is kept");
}

// ---------------------------------------------------------------- swma

/// `edma_scalping_strategy_exponentially_deviating_m`'s weighting, oldest first, as the platforms sum it.
fn swma_ref(c: &[f64]) -> Vec<f64> {
    (0..c.len())
        .map(|i| {
            if i < 3 {
                return f64::NAN;
            }
            let w = &c[i - 3..=i];
            w[0] * 1.0 / 6.0 + w[1] * 2.0 / 6.0 + w[2] * 2.0 / 6.0 + w[3] * 1.0 / 6.0
        })
        .collect()
}

#[test]
fn swma_is_the_platforms_symmetric_weighting() {
    let mut c = closes(100.0, 2.0, 1500);
    c[700] = f64::NAN;
    let b: Vec<Bar> =
        c.iter().enumerate().map(|(i, &x)| Bar::ohlcv(i as i64, x, x, x, x, 1.0)).collect();
    let want = swma_ref(&c);
    let got = checked_stream("swma", Box::new(Swma::default()), &b).remove(0);
    assert!(same(&got, &want), "swma: stream against the platforms' sum");
    assert!(same(&series::swma(&c), &want), "swma: series");
    assert!(same(&registry("swma", &[], &b)[0], &want), "swma: registry row");
    assert!(got[700..704].iter().all(|v| v.is_nan()) && !got[704].is_nan(), "a NaN leaves after 4");
    assert_eq!(Swma::default().lookback(), 3);
}

// ---------------------------------------------------------------- rsi, stochrsi, pivot points

// The rsi's RS form and saturation, the stochrsi's RS form and the pivots' Fibonacci type (set B,
// `supertrex_strategy` and `emperorbtc_s_vwap_strategy`), imported here so the other sections'
// imports stay as they are.
use vike_indicators::indicators::{PivotPoints, Rsi, Sma, Stochrsi};

/// Set B's `helpers::AlphaRma`: the plain sum of the first `length` numbers over `length`, then
/// `alpha * x + (1 - alpha) * last`; a `NaN` gives the last value.
struct AlphaRma {
    length: f64,
    seen: f64,
    sum: f64,
    last: f64,
}

impl AlphaRma {
    fn next(&mut self, x: f64) -> f64 {
        if x.is_nan() {
            return self.last;
        }
        if self.seen < self.length {
            self.seen += 1.0;
            self.sum += x;
            if self.seen == self.length {
                self.last = self.sum / self.length;
            }
        } else {
            let alpha = 1.0 / self.length;
            self.last = alpha * x + (1.0 - alpha) * self.last;
        }
        self.last
    }
}

/// Set B's `helpers::ReferenceRsi` over `AlphaRma` (the copy this option replaced), with
/// `supertrex_strategy`'s saturation as a switch: mean losses at or under 1e-10 read 100, else mean
/// gains at or under it 0.
struct ReferenceRsi {
    previous: f64,
    up: AlphaRma,
    down: AlphaRma,
    saturate: bool,
}

impl ReferenceRsi {
    fn new(length: usize, saturate: bool) -> Self {
        let rma = || AlphaRma { length: length as f64, seen: 0.0, sum: 0.0, last: f64::NAN };
        ReferenceRsi { previous: f64::NAN, up: rma(), down: rma(), saturate }
    }

    fn next(&mut self, x: f64) -> f64 {
        let (u, d) = ((x - self.previous).max(0.0), (self.previous - x).max(0.0));
        let first = self.previous.is_nan();
        self.previous = x;
        if first {
            return f64::NAN;
        }
        let (up, down) = (self.up.next(u), self.down.next(d));
        if self.saturate {
            if down <= 1e-10 {
                return 100.0;
            }
            if up <= 1e-10 {
                return 0.0;
            }
        }
        100.0 - 100.0 / (1.0 + up / down)
    }
}

/// The registry row's arithmetic written out (the seed's sum times `1 / n`, then
/// `mean * (n - 1) / n + move / n`, `100 * up / (up + down)`), the saturation a switch.
fn wilder_rsi(c: &[f64], n: usize, saturate: bool) -> Vec<f64> {
    let (inverse, keep) = (1.0 / n as f64, (n as f64 - 1.0) / n as f64);
    let (mut ag, mut al) = (0.0, 0.0);
    (0..c.len())
        .map(|i| {
            if i == 0 {
                return f64::NAN;
            }
            let d = c[i] - c[i - 1];
            let (g, l) = (d.max(0.0), (-d).max(0.0));
            if i < n {
                ag += g;
                al += l;
                return f64::NAN;
            }
            if i == n {
                (ag, al) = ((ag + g) * inverse, (al + l) * inverse);
            } else {
                (ag, al) = (ag * keep + g * inverse, al * keep + l * inverse);
            }
            if saturate && al <= 1e-10 {
                100.0
            } else if saturate && ag <= 1e-10 {
                0.0
            } else if al == 0.0 {
                100.0
            } else {
                100.0 * ag / (ag + al)
            }
        })
        .collect()
}

fn close_bars(c: &[f64]) -> Vec<Bar> {
    c.iter().enumerate().map(|(i, &x)| Bar::ohlcv(i as i64 * 60_000, x, x, x, x, 1.0)).collect()
}

/// A weekly sample read on 30-minute bars (`supertrex_strategy`'s): each close held for 336 bars,
/// so the means decay under 1e-10 between two changes.
fn held_closes() -> Vec<f64> {
    closes(100.0, 2.0, 40).iter().flat_map(|&x| [x; 336]).collect()
}

#[test]
fn rsi_options_unset_are_the_registry_row() {
    let b = crate::options::hourly_bars();
    for n in [1.0, 2.0, 14.0, 40.0] {
        let want = registry("rsi", &[n], &b);
        let off = Rsi::new(n).unwrap().with_rs_form(false).with_saturation(false);
        assert_lines_same("rsi off", &checked_stream("rsi off", Box::new(off), &b), &want);
        let mut plain = Rsi::new(n).unwrap();
        let values: Vec<f64> = b.iter().map(|x| plain.next_value(x.close)).collect();
        assert!(same(&values, &want[0]), "rsi({n}): next_value");
    }
}

/// Set, `rs_form` IS set B's `helpers::ReferenceRsi` bit for bit, leading `NaN`s skipped (as
/// `b_xtrender_backtest_edition_quanttherapy` feeds an EMA's difference once it exists), at three
/// price levels; the stream equals `vectorize` and survives `reset`. A `NaN` after the first
/// number is skipped by vike's form (`ReferenceRsi` counts it as a move of 0 and then starts over).
#[test]
fn rsi_rs_form_is_the_reference_rsi() {
    for (level, step) in [(100.0, 2.0), (1.0e6, 0.01), (1.0e-3, 1.0e-6)] {
        let mut c = vec![f64::NAN; 19];
        c.extend(closes(level, step, 4000));
        let b = close_bars(&c);
        for n in [1usize, 5, 14, 40] {
            let tag = format!("rsi({n}) rs form at {level}");
            let set = Rsi::new(n as f64).unwrap().with_rs_form(true);
            let got = checked_stream(&tag, Box::new(set), &b).remove(0);
            let mut want = ReferenceRsi::new(n, false);
            let want: Vec<f64> = c.iter().map(|&x| want.next(x)).collect();
            assert!(same(&got, &want), "{tag}");
            assert!(got[19 + n - 1].is_nan() && !got[19 + n].is_nan(), "{tag}: the first value");
            if n > 1 {
                let plain = registry("rsi", &[n as f64], &b).remove(0);
                assert!(!same(&got[19..], &plain[19..]), "{tag}: the form changed nothing");
            }
        }
    }
    let mut gap = Rsi::new(3.0).unwrap().with_rs_form(true);
    let mut whole = Rsi::new(3.0).unwrap().with_rs_form(true);
    let c = [10.0, 11.0, 9.5, 12.0, 12.5, 11.0];
    for (i, &x) in c.iter().enumerate() {
        if i == 4 {
            assert!(gap.next_value(f64::NAN).is_nan(), "a NaN reads NaN");
        }
        assert_eq!(bits(gap.next_value(x)), bits(whole.next_value(x)), "a NaN moves nothing");
    }
}

/// Set, `saturation` reads mean losses at or under 1e-10 as 100 and else mean gains at or under it as
/// 0, in either form: on the RS form `supertrex_strategy`'s rule over `ReferenceRsi`'s means, on the
/// registry row's form its means written out, both bit for bit over a weekly sample on 30-minute bars,
/// where the means of both sides decay under 1e-10 and the unset readings are anything from 0 to 100.
#[test]
fn rsi_saturation_is_the_reference_rsis() {
    let c = held_closes();
    let b = close_bars(&c);
    for n in [2usize, 14] {
        let rs = Rsi::new(n as f64).unwrap().with_rs_form(true).with_saturation(true);
        let got = checked_stream("rs saturated", Box::new(rs), &b).remove(0);
        let mut want = ReferenceRsi::new(n, true);
        let want: Vec<f64> = c.iter().map(|&x| want.next(x)).collect();
        assert!(same(&got, &want), "rsi({n}) rs form, saturated");
        let mut unset = ReferenceRsi::new(n, false);
        let unset: Vec<f64> = c.iter().map(|&x| unset.next(x)).collect();
        let moved = got.iter().zip(&unset).filter(|(g, u)| bits(**g) != bits(**u)).count();
        assert!(moved > 1000, "rsi({n}) rs form: only {moved} bars saturate");

        let wilder = Rsi::new(n as f64).unwrap().with_saturation(true);
        let got = checked_stream("saturated", Box::new(wilder), &b).remove(0);
        assert!(same(&got, &wilder_rsi(&c, n, true)), "rsi({n}) saturated");
        let row = registry("rsi", &[n as f64], &b).remove(0);
        assert!(same(&row, &wilder_rsi(&c, n, false)), "rsi({n}): the row's arithmetic");
        assert!(!same(&got, &row), "rsi({n}): saturation changed nothing");
    }
    let mut flat = Rsi::new(2.0).unwrap().with_rs_form(true).with_saturation(true);
    let read: Vec<f64> = [5.0, 5.0, 5.0, 5.0].map(|x| flat.next_value(x)).into();
    assert_eq!(read[2..], [100.0, 100.0], "no move at all reads 100, not NaN");
}

/// Set, `rs_form` takes the stochastic of `ReferenceRsi`: `%K` is `emperorbtc_s_vwap_strategy`'s
/// `100 * (rsi - lowest) / (highest - lowest)` over the last `k` RSI values (its `Donchian`) bit for
/// bit once those are all numbers, and `%D` is vike's `Sma` of the `%K` numbers. Before that the
/// strategy's window read the RSI values it had (from the second one); vike's reads `NaN`.
#[test]
fn stochrsi_rs_form_is_the_stochastic_of_the_reference_rsi() {
    for (level, step) in [(100.0, 2.0), (1.0e6, 0.01)] {
        let c = closes(level, step, 5000);
        let b = close_bars(&c);
        // No 1-value window: its range is always flat, which vike reads as 0 and the strategy as `NaN`.
        for (rsi_p, k, d) in [(14usize, 14usize, 3usize), (5, 9, 4), (7, 2, 1)] {
            let tag = format!("stochrsi({rsi_p}, {k}, {d}) at {level}");
            let set = Stochrsi::new(rsi_p as f64, k as f64, d as f64).unwrap().with_rs_form(true);
            let got = checked_stream(&tag, Box::new(set), &b);
            let mut rsi = ReferenceRsi::new(rsi_p, false);
            let mut range = Donchian::new(k as f64).unwrap();
            let mut smooth = Sma::new(d as f64).unwrap();
            let full = rsi_p + k - 1;
            for (i, &x) in c.iter().enumerate() {
                let r = rsi.next(x);
                let [high, _, low] = range.next_hl(r, r);
                let lab = 100.0 * (r - low) / (high - low);
                if i < full {
                    assert!(got[0][i].is_nan() && got[1][i].is_nan(), "{tag}: bar {i} warms up");
                    continue;
                }
                assert_eq!(bits(got[0][i]), bits(lab), "{tag} %K, bar {i}");
                assert_eq!(bits(got[1][i]), bits(smooth.next_value(lab)), "{tag} %D, bar {i}");
            }
            let plain = registry("stochrsi", &[rsi_p as f64, k as f64, d as f64], &b);
            assert!(!same(&got[0], &plain[0]), "{tag}: the form changed nothing");
        }
    }
    let b = crate::options::hourly_bars();
    let off = Stochrsi::new(14.0, 14.0, 3.0).unwrap().with_rs_form(false);
    assert_lines_same(
        "stochrsi off",
        &checked_stream("stochrsi off", Box::new(off), &b),
        &registry("stochrsi", &[14.0, 14.0, 3.0], &b),
    );
}

/// Set, `fibonacci` gives `emperorbtc_s_vwap_strategy`'s levels of the prior bar bit for bit
/// (`pp - (h - l)`, `pp - 0.618 * (h - l)`, `pp - 0.382 * (h - l)`, `pp`, and their mirror above),
/// `NaN` on the first bar; the stream equals `vectorize` and survives `reset`; unset, the row.
#[test]
fn pivot_points_fibonacci_is_the_strategys_levels() {
    let b = bodied_bars(&closes(100.0, 2.0, 3000));
    let set = PivotPoints::default().with_fibonacci(true);
    let got = checked_stream("fibonacci pivots", Box::new(set), &b);
    assert!(got.iter().all(|line| line[0].is_nan()), "no prior bar");
    for (i, w) in b.windows(2).enumerate() {
        let (h, l, c) = (w[0].high, w[0].low, w[0].close);
        let pp = (h + l + c) / 3.0;
        let want = [
            pp,
            pp + 0.382 * (h - l),
            pp + 0.618 * (h - l),
            pp + (h - l),
            pp - 0.382 * (h - l),
            pp - 0.618 * (h - l),
            pp - (h - l),
        ];
        for (line, x) in got.iter().zip(want) {
            assert_eq!(bits(line[i + 1]), bits(x), "bar {}", i + 1);
        }
    }
    let off = PivotPoints::default().with_fibonacci(false);
    let row = registry("pivot_points", &[], &b);
    assert_lines_same("pivots off", &checked_stream("pivots off", Box::new(off), &b), &row);
    assert!(!same(&got[1], &row[1]), "fibonacci changed nothing");
    let mut levels = PivotPoints::default().with_fibonacci(true);
    levels.next_hlc(12.0, 2.0, 7.0);
    assert_eq!(levels.next_hlc(f64::NAN, f64::NAN, f64::NAN)[3], 17.0, "p 7, r3 p + 10");
}

// ---------------------------------------------------------------- atr

/// `macd_strategy`'s ATR (desk 2): no true range without a previous close, then
/// `atr + (tr - atr) / n`, a `NaN` step taking the bar's own range.
fn seeded_atr(b: &[Bar], n: f64) -> Vec<f64> {
    let (mut pc, mut atr) = (f64::NAN, f64::NAN);
    b.iter()
        .map(|bar| {
            let tr = if pc.is_nan() {
                f64::NAN
            } else {
                (bar.high - bar.low).max((bar.high - pc).abs().max((bar.low - pc).abs()))
            };
            pc = bar.close;
            let next = atr + (tr - atr) / n;
            atr = if next.is_nan() { tr } else { next };
            atr
        })
        .collect()
}

#[test]
fn atr_seed_first_is_the_first_true_range_then_the_rules_step() {
    let mut sets = vec![crate::options::hourly_bars(), awkward_bars(3000, 400..460, 0..0)];
    sets.push(bodied_bars(&closes(1.0e6, 0.01, 4000)));
    for b in &sets {
        for n in [1.0, 14.0, 17.0, 200.0] {
            let plain = checked_stream("atr", Box::new(Atr::new(n).unwrap()), b);
            let off = Atr::new(n).unwrap().with_seed_first(false);
            assert_lines_same("atr off", &checked_stream("atr off", Box::new(off), b), &plain);
            assert_lines_same("atr", &plain, &registry("atr", &[n], b));
            let set = Atr::new(n).unwrap().with_seed_first(true);
            assert_eq!(set.lookback(), 1);
            let got = checked_stream("atr seeded", Box::new(set), b).remove(0);
            assert!(same(&got, &seeded_atr(b, n)), "atr({n}) seed_first");
            assert!(got[0].is_nan() && !got[1].is_nan(), "atr({n}): the value starts on bar 1");
            let mut plain_numbers = Atr::new(n).unwrap().with_seed_first(true);
            let hlc: Vec<f64> =
                b.iter().map(|x| plain_numbers.next_hlc(x.high, x.low, x.close)).collect();
            assert!(same(&hlc, &got), "atr({n}) seed_first: next_hlc");
        }
    }
}

// ---------------------------------------------------------------- donchian restart

/// `auto_fib_golden_pocket_band_autofib_moving_avera`'s `Tail` (level 3, batch 4): the extreme of the
/// last `length` values since the last missing one, `NaN` at a missing value and until `length` bars
/// have come.
fn tail(x: &[f64], length: usize, low: bool) -> Vec<f64> {
    let mut values = std::collections::VecDeque::new();
    x.iter()
        .enumerate()
        .map(|(i, &v)| {
            if v.is_nan() {
                values.clear();
                return f64::NAN;
            }
            values.push_back(v);
            if values.len() > length {
                values.pop_front();
            }
            if i + 1 < length {
                return f64::NAN;
            }
            let fold = if low { f64::min } else { f64::max };
            values.iter().copied().reduce(fold).unwrap()
        })
        .collect()
}

#[test]
fn donchian_restart_at_missing_is_the_platforms_lowest_and_highest_over_gaps() {
    let mut x = closes(100.0, 2.0, 9000);
    for gap in [30..34, 500..501, 4100..4300, 7000..7001] {
        x[gap].fill(f64::NAN);
    }
    let b: Vec<Bar> =
        x.iter().enumerate().map(|(i, &v)| Bar::ohlcv(i as i64, v, v, v, v, 1.0)).collect();
    for n in [1usize, 5, 150, 4000] {
        let d = Donchian::new(n as f64).unwrap().with_restart_at_missing(true);
        let got = checked_stream("donchian restart", Box::new(d), &b);
        let (hi, lo) = (tail(&x, n, false), tail(&x, n, true));
        assert!(same(&got[0], &hi) && same(&got[2], &lo), "donchian({n}) restart: the extremes");
        let mid: Vec<f64> = hi.iter().zip(&lo).map(|(h, l)| (h + l) * 0.5).collect();
        assert!(same(&got[1], &mid), "donchian({n}) restart: the middle");
        let plain = Donchian::new(n as f64).unwrap().with_restart_at_missing(false);
        let plain = checked_stream("donchian", Box::new(plain), &b);
        assert_lines_same("donchian unset", &plain, &registry("donchian", &[n as f64], &b));
        if n > 1 {
            assert!(!same(&got[0], &plain[0]), "donchian({n}): restarting changed nothing");
        }
    }
    assert!(Donchian::new(5000.0).is_ok() && Donchian::new(5001.0).is_err());
}

// ---------------------------------------------------------------- psar extreme first

/// `easy_sys_420`'s `sar_point` (level 3, batch 11), its constants made parameters: the extreme and
/// the factor taken first, the move, the clamp to the last two bars, then the reversal test.
struct ExtremeFirst {
    start: f64,
    step: f64,
    cap: f64,
    /// `(uptrend, newtrend, ep, af, sar)` the last bar left.
    previous: Option<(bool, bool, f64, f64, f64)>,
    /// `(high, low)` one and two bars back.
    back: [(f64, f64); 2],
}

impl ExtremeFirst {
    fn next(&mut self, h: f64, l: f64) -> (f64, bool) {
        let [(h1, l1), (h2, l2)] = self.back;
        let (mut uptrend, mut newtrend, mut ep, mut af, mut sar) = (true, false, h, self.start, l);
        if let Some((p_up, p_new, p_ep, p_af, p_sar)) = self.previous {
            ep = if p_up { h.max(p_ep) } else { l.min(p_ep) };
            af = if p_new {
                self.start
            } else if ep != p_ep {
                self.cap.min(p_af + self.step)
            } else {
                p_af
            };
            sar = p_sar + af * (ep - p_sar);
            if p_up {
                sar = sar.min(l1);
                if !l2.is_nan() {
                    sar = sar.min(l2);
                }
                if sar > l {
                    (uptrend, newtrend) = (false, true);
                    sar = h.max(p_ep);
                    ep = l.min(l1);
                }
            } else {
                sar = sar.max(h1);
                if !h2.is_nan() {
                    sar = sar.max(h2);
                }
                if sar < h {
                    (uptrend, newtrend) = (true, true);
                    sar = l.min(p_ep);
                    ep = h.max(h1);
                } else {
                    uptrend = false;
                }
            }
        }
        self.previous = Some((uptrend, newtrend, ep, af, sar));
        self.back = [(h, l), (h1, l1)];
        (sar, uptrend)
    }
}

#[test]
fn psar_extreme_first_is_easy_sys_420s_sar() {
    let mut sets: Vec<(String, Vec<Bar>)> = Vec::new();
    for (level, step) in [(100.0, 2.0), (1.0e6, 0.01), (1.0e-3, 1.0e-6)] {
        sets.push((format!("walk at {level}"), bodied_bars(&closes(level, step, 6000))));
    }
    sets.push(("awkward".to_string(), awkward_bars(3000, 400..460, 0..0)));
    for (start, step, cap) in [(0.02, 0.02, 0.2), (0.03, 0.01, 0.025)] {
        for (name, b) in &sets {
            let tag = format!("{name} ({start}, {step}, {cap})");
            let build = || {
                let p = Psar::new(step, cap).unwrap().with_start(start).unwrap();
                p.with_extreme_first(true)
            };
            checked_stream(&tag, Box::new(build()), b);
            let n = f64::NAN;
            let mut want = ExtremeFirst { start, step, cap, previous: None, back: [(n, n); 2] };
            let mut got = build();
            let mut flips = 0usize;
            let mut was = true;
            for (i, bar) in b.iter().enumerate() {
                let (w, up) = want.next(bar.high, bar.low);
                assert_eq!(bits(got.next(bar)), bits(w), "{tag}, bar {i}");
                assert_eq!(got.uptrend(), Some(up), "{tag} uptrend, bar {i}");
                assert!(got.stop_ahead().is_nan(), "{tag}: no stop ahead");
                flips += usize::from(up != was);
                was = up;
            }
            if name == "walk at 100" {
                assert!(flips > 50, "{tag}: only {flips} flips, the series tests little");
            }
        }
    }
    let b = bodied_bars(&closes(100.0, 2.0, 1500));
    let set = Psar::default().with_extreme_first(true);
    let got = checked_stream("extreme first", Box::new(set.clone()), &b);
    assert!(
        !same(&got[0], &registry("psar", &[0.02, 0.2], &b)[0]),
        "extreme first changed nothing"
    );
    // The other options do not reach it: the first bar keeps its value.
    let mixed =
        set.with_direction_from_closes(true).with_test_first(true).with_textbook_steps(true);
    assert_lines_same(
        "extreme first, others set",
        &checked_stream("mixed", Box::new(mixed), &b),
        &got,
    );
}

// ------------------------------------------- adx, supertrend, chandelier (level 3, wave 2)

use crate::fast::col;
use vike_indicators::indicators::{Adx, Chandelier, Supertrend};

/// The series the wave-2 options run over: hourly bars, awkward ones (a flat stretch, a jump, a
/// zero close), bodied walks at a large and a small price, and bars flat for their first 20.
pub(crate) fn wave_two_sets() -> Vec<(String, Vec<Bar>)> {
    let mut sets = vec![
        ("hourly".to_string(), hourly_bars()),
        ("awkward".to_string(), awkward_bars(3000, 400..460, 0..0)),
        ("walk at 1e6".to_string(), bodied_bars(&closes(1.0e6, 0.01, 4000))),
        ("walk at 100".to_string(), bodied_bars(&closes(100.0, 2.0, 4000))),
    ];
    let mut flat = bodied_bars(&closes(100.0, 2.0, 1500));
    for (i, bar) in flat.iter_mut().enumerate().take(20) {
        *bar = Bar::ohlcv(i as i64 * 60_000, 100.0, 100.0, 100.0, 100.0, 1.0);
    }
    sets.push(("flat start".to_string(), flat));
    sets
}

/// The lab's compensated rolling sum (`strategy_lab_b`'s `RollingSum`): the sum of the last
/// `length` numbers, a `NaN` skipped, `NaN` until `length` have come.
struct RollingSum {
    length: usize,
    window: std::collections::VecDeque<f64>,
    sum: f64,
    carry: f64,
}

impl RollingSum {
    fn new(length: usize) -> Self {
        RollingSum { length, window: Default::default(), sum: 0.0, carry: 0.0 }
    }

    fn next(&mut self, x: f64) -> f64 {
        let mut add = |v: f64| {
            let t = self.sum + v;
            self.carry +=
                if self.sum.abs() >= v.abs() { (self.sum - t) + v } else { (v - t) + self.sum };
            self.sum = t;
        };
        if !x.is_nan() {
            if self.window.len() == self.length {
                add(-self.window.pop_front().unwrap());
            }
            add(x);
            self.window.push_back(x);
        }
        if self.window.len() == self.length { self.sum + self.carry } else { f64::NAN }
    }
}

/// `coral_trend_pullback_strategy_tradeiq`'s ADX (level 3, batch 3), its length a parameter:
/// `[adx, +DI, -DI]`.
struct CoralAdx {
    n: f64,
    previous: (f64, f64, f64),
    sums: (f64, f64, f64),
    mean: RollingSum,
}

impl CoralAdx {
    fn next(&mut self, bar: &Bar) -> [f64; 3] {
        let (high, low, close) = self.previous;
        let up = bar.high - high;
        let down = low - bar.low;
        let true_range =
            (bar.high - bar.low).max((bar.high - close).abs()).max((bar.low - close).abs());
        let (dm_plus, dm_minus) = (
            if up > down { up.max(0.0) } else { 0.0 },
            if down > up { down.max(0.0) } else { 0.0 },
        );
        self.previous = (bar.high, bar.low, bar.close);
        let n = self.n;
        let smooth = |sum: f64, x: f64| sum - sum / n + x;
        let (range, plus, minus) = self.sums;
        self.sums = (smooth(range, true_range), smooth(plus, dm_plus), smooth(minus, dm_minus));
        let (range, plus, minus) = self.sums;
        let (di_plus, di_minus) = (plus / range * 100.0, minus / range * 100.0);
        let dx = (di_plus - di_minus).abs() / (di_plus + di_minus) * 100.0;
        [self.mean.next(dx) / n, di_plus, di_minus]
    }
}

/// Set, `zero_seeded_sums` IS coral's ADX: the DI lines bit for bit from the first bar, the ADX (its
/// mean a left fold where coral's was a compensated rolling sum) to that sum's rounding and `NaN`
/// exactly where coral's is; the stream equals `vectorize`, survives `reset` and `with_adx_period`.
#[test]
fn adx_zero_seeded_sums_is_coral_trends_adx() {
    for (name, b) in &wave_two_sets() {
        for n in [2usize, 5, 14, 30] {
            let set = Adx::new(n as f64).unwrap().with_zero_seeded_sums(true);
            assert_eq!(set.lookback(), 0);
            let got = checked_stream(name, Box::new(set), b);
            let zero = (0.0, 0.0, 0.0);
            let mut coral =
                CoralAdx { n: n as f64, previous: zero, sums: zero, mean: RollingSum::new(n) };
            let want: Vec<[f64; 3]> = b.iter().map(|bar| coral.next(bar)).collect();
            for k in [1, 2] {
                assert!(same(&got[k], &col(&want, k)), "{name} adx({n}) zero-seeded: line {k}");
            }
            assert_close(&format!("{name} adx({n}) zero"), &got[0], &col(&want, 0), 1e-12, 1.0);
            assert!(got[0][n - 1].is_finite() && got[0][..n - 1].iter().all(|v| v.is_nan()));
        }
    }
    let b = hourly_bars();
    let off = Adx::new(14.0).unwrap().with_zero_seeded_sums(false).with_first_bar_moves(false);
    let want = registry("adx", &[14.0], &b);
    assert_lines_same("adx off", &checked_stream("adx off", Box::new(off), &b), &want);
    let set = Adx::new(14.0).unwrap().with_zero_seeded_sums(true);
    let got = checked_stream("adx zero", Box::new(set.clone()), &b);
    assert!(!same(&got[0], &want[0]) && !same(&got[1], &want[1]), "zero seeding changed nothing");
    let kept = checked_stream("adx zero, period", Box::new(set.with_adx_period(14.0).unwrap()), &b);
    assert_lines_same("with_adx_period keeps the option", &kept, &got);
}

/// The lab's Wilder mean (`strategy_lab_b`'s `helpers::Rma`): the mean of the first `length`
/// numbers (their compensated sum), then `(last * (length - 1) + x) / length`; a `NaN` gives `NaN`
/// and moves nothing.
struct LabRma {
    length: f64,
    seed: RollingSum,
    last: f64,
}

impl LabRma {
    fn new(length: usize) -> Self {
        LabRma { length: length as f64, seed: RollingSum::new(length), last: f64::NAN }
    }

    fn next(&mut self, x: f64) -> f64 {
        if x.is_nan() {
            return f64::NAN;
        }
        self.last = if self.last.is_nan() {
            self.seed.next(x) / self.length
        } else {
            (self.last * (self.length - 1.0) + x) / self.length
        };
        self.last
    }
}

/// `easy_sys_420`'s directional index (level 3, batch 11), its length a parameter: a move against
/// the missing bar before the first is no move (0), the first bar has no true range, the lines are
/// `100 * rma(move) / rma(range)`, each held at its last number. `(+DI, -DI)`.
struct EasyDi {
    previous: Option<(f64, f64, f64)>,
    range: LabRma,
    plus: LabRma,
    minus: LabRma,
    held: (f64, f64),
}

impl EasyDi {
    fn next(&mut self, bar: &Bar) -> (f64, f64) {
        let n = f64::NAN;
        let (h, l, c) = self.previous.replace((bar.high, bar.low, bar.close)).unwrap_or((n, n, n));
        let (up, down) = (bar.high - h, -(bar.low - l));
        let true_range = if c.is_nan() {
            f64::NAN
        } else {
            (bar.high - bar.low).max((bar.high - c).abs()).max((bar.low - c).abs())
        };
        let range = self.range.next(true_range);
        let plus = 100.0 * self.plus.next(if up > down && up > 0.0 { up } else { 0.0 }) / range;
        let minus =
            100.0 * self.minus.next(if down > up && down > 0.0 { down } else { 0.0 }) / range;
        if !plus.is_nan() {
            self.held.0 = plus;
        }
        if !minus.is_nan() {
            self.held.1 = minus;
        }
        self.held
    }
}

/// Set with `held_lines`, `first_bar_moves` IS easy_sys_420's DI lines: to the rounding of its means
/// (Wilder's sums here), `NaN` exactly where its lines are, a flat start held; the stream equals
/// `vectorize` and survives `reset`. Unset, nothing moves.
#[test]
fn adx_first_bar_moves_is_easy_sys_420s_di_lines() {
    for (name, b) in &wave_two_sets() {
        for n in [2usize, 5, 14] {
            let set = Adx::new(n as f64).unwrap().with_first_bar_moves(true).with_held_lines(true);
            let got = checked_stream(name, Box::new(set), b);
            let rma = || LabRma::new(n);
            let held = (f64::NAN, f64::NAN);
            let mut easy = EasyDi { previous: None, range: rma(), plus: rma(), minus: rma(), held };
            let (plus, minus): (Vec<f64>, Vec<f64>) = b.iter().map(|bar| easy.next(bar)).unzip();
            assert_close(&format!("{name} +DI({n}) first moves"), &got[1], &plus, 1e-12, 1.0);
            assert_close(&format!("{name} -DI({n}) first moves"), &got[2], &minus, 1e-12, 1.0);
        }
    }
    let b = hourly_bars();
    let want = registry("adx", &[5.0], &b);
    let set = Adx::new(5.0).unwrap().with_first_bar_moves(true);
    let got = checked_stream("adx first", Box::new(set), &b);
    assert!(!same(&got[1], &want[1]), "first bar moves changed nothing");
    assert_eq!(got[1].iter().position(|v| !v.is_nan()), want[1].iter().position(|v| !v.is_nan()));
}

/// `heikin_ashi_supertrend`'s Supertrend (level 3, batch 11), run on plain bars, its length and
/// factor parameters: `[line, direction]`, the line the lower band while the trend is up.
struct PreviousBands {
    atr: Atr,
    mult: f64,
    last_close: f64,
    up: f64,
    down: f64,
    direction: i32,
}

impl PreviousBands {
    fn next(&mut self, bar: &Bar) -> [f64; 2] {
        let atr = self.atr.next(bar);
        let last_close = std::mem::replace(&mut self.last_close, bar.close);
        let middle = (bar.high + bar.low) / 2.0;
        let (band_up, band_down) = (middle - self.mult * atr, middle + self.mult * atr);
        let trending_up = if last_close > self.up { band_up.max(self.up) } else { band_up };
        let trending_down =
            if last_close < self.down { band_down.min(self.down) } else { band_down };
        let direction = if bar.close > self.down {
            1
        } else if bar.close < self.up {
            -1
        } else if self.direction == 0 {
            1
        } else {
            self.direction
        };
        (self.up, self.down, self.direction) = (trending_up, trending_down, direction);
        [if direction == 1 { trending_up } else { trending_down }, f64::from(direction)]
    }
}

/// Set, `previous_bands` IS heikin_ashi_supertrend's Supertrend bit for bit from the first bar,
/// both lines; the line starts at `lookback`; the stream equals `vectorize` and survives `reset`.
#[test]
fn supertrend_previous_bands_is_heikin_ashi_supertrends() {
    for (name, b) in &wave_two_sets() {
        for (n, mult) in [(10.0, 2.7), (7.0, 3.0), (1.0, 0.5)] {
            let tag = format!("{name} supertrend({n}, {mult}) previous bands");
            let set = Supertrend::new(n, mult).unwrap().with_previous_bands(true);
            let lookback = set.lookback();
            let got = checked_stream(&tag, Box::new(set), b);
            let atr = Atr::new(n).unwrap();
            let n = f64::NAN;
            let mut want = PreviousBands { atr, mult, last_close: n, up: n, down: n, direction: 0 };
            let want: Vec<[f64; 2]> = b.iter().map(|bar| want.next(bar)).collect();
            assert!(same(&got[0], &col(&want, 0)) && same(&got[1], &col(&want, 1)), "{tag}");
            assert_eq!(got[0].iter().position(|v| !v.is_nan()), Some(lookback), "{tag}");
            let flips = got[1].windows(2).filter(|w| w[0] != w[1]).count();
            assert!(name != "walk at 100" || flips > 50, "{tag}: only {flips} flips");
        }
    }
    let b = hourly_bars();
    let want = registry("supertrend", &[10.0, 3.0], &b);
    let off = Supertrend::new(10.0, 3.0).unwrap().with_previous_bands(false);
    assert_lines_same("supertrend off", &checked_stream("off", Box::new(off), &b), &want);
    let set = Supertrend::new(10.0, 3.0).unwrap().with_previous_bands(true);
    assert!(!same(&checked_stream("set", Box::new(set), &b)[1], &want[1]), "changed nothing");
}

/// `supertrend_multi_time_frame_long_and_short_tradi`'s chandelier (level 3, batch 9), its length,
/// factor, extremes and ratchet parameters: `([long_stop, short_stop], uptrend)`.
struct RuleChandelier {
    atr: Atr,
    extremes: Donchian,
    mult: f64,
    closes: bool,
    ratchet: bool,
    long_stop: f64,
    short_stop: f64,
    previous_close: f64,
    dir: i32,
}

impl RuleChandelier {
    fn new(n: f64, mult: f64, closes: bool, ratchet: bool) -> Self {
        let (atr, extremes) = (Atr::new(n).unwrap(), Donchian::new(n).unwrap());
        let (long_stop, short_stop, previous_close, dir) = (f64::NAN, f64::NAN, f64::NAN, 1);
        RuleChandelier {
            atr,
            extremes,
            mult,
            closes,
            ratchet,
            long_stop,
            short_stop,
            previous_close,
            dir,
        }
    }

    fn next(&mut self, bar: &Bar) -> ([f64; 2], bool) {
        let nan_max = |a: f64, b: f64| if a.is_nan() || b.is_nan() { f64::NAN } else { a.max(b) };
        let nan_min = |a: f64, b: f64| if a.is_nan() || b.is_nan() { f64::NAN } else { a.min(b) };
        let atr = self.mult * self.atr.next(bar);
        let [highest, _, lowest] = if self.closes {
            self.extremes.next_hl(bar.close, bar.close)
        } else {
            self.extremes.next_hl(bar.high, bar.low)
        };
        let long_stop = highest - atr;
        let long_previous = if self.long_stop.is_nan() { long_stop } else { self.long_stop };
        let long_stop = if self.ratchet && self.previous_close > long_previous {
            nan_max(long_stop, long_previous)
        } else {
            long_stop
        };
        let short_stop = lowest + atr;
        let short_previous = if self.short_stop.is_nan() { short_stop } else { self.short_stop };
        let short_stop = if self.ratchet && self.previous_close < short_previous {
            nan_min(short_stop, short_previous)
        } else {
            short_stop
        };
        self.dir = if bar.close > short_previous {
            1
        } else if bar.close < long_previous {
            -1
        } else {
            self.dir
        };
        (self.long_stop, self.short_stop, self.previous_close) = (long_stop, short_stop, bar.close);
        ([long_stop, short_stop], self.dir == 1)
    }
}

/// `close_extremes` and `ratchet`, each and both, ARE the rule's chandelier bit for bit, its trend
/// included (unset both, the registry row's stops); the stream equals `vectorize` and survives
/// `reset`.
#[test]
fn chandelier_close_extremes_and_ratchet_are_the_rules_chandelier() {
    for (name, b) in &wave_two_sets() {
        for (n, mult) in [(22.0, 3.0), (5.0, 1.5), (2.0, 2.0)] {
            let unset = registry("chandelier", &[n, mult], b);
            for (closes, ratchet) in [(false, false), (true, false), (false, true), (true, true)] {
                let tag = format!("{name} chandelier({n}, {mult}) closes {closes}, {ratchet}");
                let build = || {
                    let c = Chandelier::new(n, mult).unwrap();
                    c.with_close_extremes(closes).with_ratchet(ratchet)
                };
                let got = checked_stream(&tag, Box::new(build()), b);
                let mut want = RuleChandelier::new(n, mult, closes, ratchet);
                let mut stream = build();
                let mut downs = 0usize;
                for (i, bar) in b.iter().enumerate() {
                    let (w, up) = want.next(bar);
                    let g = stream.next(bar).map(bits);
                    assert_eq!(g, w.map(bits), "{tag}, bar {i}");
                    assert_eq!(stream.uptrend(), up, "{tag} uptrend, bar {i}");
                    downs += usize::from(!up);
                }
                // Two bars of a walk whose steps are under its ranges never close under the stop.
                let walk = name == "walk at 100" && n >= 5.0;
                assert!(!walk || downs > 100, "{tag}: down {downs} bars");
                let moved = !same(&got[0], &unset[0]) || !same(&got[1], &unset[1]);
                assert_eq!(moved, closes || ratchet, "{tag}: against the registry row");
            }
        }
    }
}

// ------------------------- adx, supertrend, smma and rsi: the lab's other copies (level 3, wave 2)

use vike_indicators::indicators::Smma;

/// A lab average as a closure: `strategy_lab_b`'s `helpers::Rma` (kind 0, `LabRma`),
/// `helpers::AlphaRma` (kind 1), `helpers::RestartingRma` (kind 2: the plain mean of the last `n`
/// once none of them is `NaN`, then `alpha * x + (1 - alpha) * last`; a `NaN` gives `NaN` and the
/// mean waits for `n` numbers in a row again) or `Smma` skipping a `NaN` (kind 3,
/// `dax_shooter_5m_strategy`'s `rma`).
fn lab_average(kind: usize, n: usize) -> Box<dyn FnMut(f64) -> f64> {
    match kind {
        0 => {
            let mut rma = LabRma::new(n);
            Box::new(move |x| rma.next(x))
        }
        1 => {
            let mut rma = AlphaRma { length: n as f64, seen: 0.0, sum: 0.0, last: f64::NAN };
            Box::new(move |x| rma.next(x))
        }
        3 => {
            let mut rma = Smma::new(n as f64).unwrap().with_skip_nan(true);
            Box::new(move |x| rma.next_value(x))
        }
        _ => {
            let (alpha, mut window, mut last) = (1.0 / n as f64, Vec::new(), f64::NAN);
            Box::new(move |x: f64| {
                window.push(x);
                if window.len() > n {
                    window.remove(0);
                }
                last = if !last.is_nan() {
                    alpha * x + (1.0 - alpha) * last
                } else if window.len() == n {
                    window.iter().sum::<f64>() / n as f64
                } else {
                    f64::NAN
                };
                last
            })
        }
    }
}

type Average = Box<dyn FnMut(f64) -> f64>;

/// The true range against the last close, as the lab's rules write it.
fn lab_range(bar: &Bar, close: f64) -> f64 {
    (bar.high - bar.low).max((bar.high - close).abs()).max((bar.low - close).abs())
}

/// The lab's ADX whose DI lines hold: `helpers::AlphaAdx` and
/// `bollinger_bands_rsi_and_adx_trading_system`'s (over `AlphaRma`), `triple_ema_trend`'s and
/// `rob_booker_adx_breakout`'s (over `Rma`), `golden_vector_trend_orchestrator_gvto`'s (over
/// `RestartingRma`), `dax_shooter_5m_strategy`'s (over `Smma`), and `keltner_channel_eth_usdt_1h`'s
/// DI lines (rob_booker's form). Averages of
/// the moves and of the true range, the range missing on the first bar and the moves too unless
/// `first_moves` (then 0: those rules compare a missing move), each DI held at its last number, and
/// `100 *` the average of `|+DI - -DI| / (+DI + -DI)`, a zero sum read as 1. `[adx, +DI, -DI]`.
struct HeldAdx {
    first_moves: bool,
    previous: Option<(f64, f64, f64)>,
    range: Average,
    plus: Average,
    minus: Average,
    adx: Average,
    di: (f64, f64),
}

impl HeldAdx {
    fn new(kind: usize, n: usize, first_moves: bool) -> Self {
        let avg = || lab_average(kind, n);
        let (range, plus, minus, adx, di) = (avg(), avg(), avg(), avg(), (f64::NAN, f64::NAN));
        HeldAdx { first_moves, previous: None, range, plus, minus, adx, di }
    }

    fn next(&mut self, bar: &Bar) -> [f64; 3] {
        let ((plus, minus), range) = match self.previous.replace((bar.high, bar.low, bar.close)) {
            None => {
                let none = if self.first_moves { 0.0 } else { f64::NAN };
                ((none, none), f64::NAN)
            }
            Some((high, low, close)) => {
                let (up, down) = (bar.high - high, low - bar.low);
                let plus = if up > down && up > 0.0 { up } else { 0.0 };
                let minus = if down > up && down > 0.0 { down } else { 0.0 };
                ((plus, minus), lab_range(bar, close))
            }
        };
        let range = (self.range)(range);
        let plus = 100.0 * (self.plus)(plus) / range;
        let minus = 100.0 * (self.minus)(minus) / range;
        if plus.is_finite() {
            self.di.0 = plus;
        }
        if minus.is_finite() {
            self.di.1 = minus;
        }
        let (plus, minus) = self.di;
        let sum = plus + minus;
        let ratio = (plus - minus).abs() / if sum == 0.0 { 1.0 } else { sum };
        [100.0 * (self.adx)(ratio), plus, minus]
    }
}

/// Set, `held_lines` IS the lab's ADX whose DI lines hold, over each of its averages, the first
/// bar's moves missing or (with `first_bar_moves`) 0: every line to the rounding of the averages
/// (Wilder's sums here), `NaN` exactly where the lab's is (a flat start's lines, its DX left out of
/// the ADX); the stream equals `vectorize`, survives `reset` and `with_adx_period`. Unset, the new
/// options are the registry row; set, a flat start moves it.
#[test]
fn adx_held_lines_is_the_labs_held_adx() {
    let sets = wave_two_sets();
    for (name, b) in &sets {
        for n in [2usize, 5, 14, 22] {
            for (kind, first_moves) in [(0, false), (1, false), (2, false), (3, false), (0, true)] {
                let tag = format!("{name} adx({n}) held, kind {kind}, first moves {first_moves}");
                let set = Adx::new(n as f64).unwrap().with_first_bar_moves(first_moves);
                let got = checked_stream(&tag, Box::new(set.with_held_lines(true)), b);
                let mut lab = HeldAdx::new(kind, n, first_moves);
                let want: Vec<[f64; 3]> = b.iter().map(|bar| lab.next(bar)).collect();
                for (k, line) in got.iter().enumerate() {
                    assert_close(&format!("{tag}: line {k}"), line, &col(&want, k), 1e-12, 1.0);
                }
            }
        }
    }
    let b = hourly_bars();
    let off = Adx::new(14.0).unwrap().with_held_lines(false).with_missing_ratios(false);
    let off = off.with_first_bar_range(false).with_dx_from_first_bar(false);
    let want = registry("adx", &[14.0], &b);
    assert_lines_same("adx off", &checked_stream("adx off", Box::new(off), &b), &want);
    let (_, flat) = sets.iter().find(|(name, _)| name == "flat start").unwrap();
    let held = Adx::new(14.0).unwrap().with_held_lines(true);
    let got = checked_stream("adx held", Box::new(held.clone()), flat);
    assert!(!same(&got[1], &registry("adx", &[14.0], flat)[1]), "held lines changed nothing");
    let period = held.with_adx_period(14.0).unwrap();
    let kept = checked_stream("adx held, period", Box::new(period), flat);
    assert_lines_same("with_adx_period keeps held lines", &kept, &got);
}

/// `crossing_moving_averages_with_adx_filter`'s ADX (level 2), its length a parameter: the first
/// bar's moves 0 and its true range `high - low`, averages that start over at a `NaN` (its own
/// `Rma`, `RestartingRma`'s law), the DI lines `100 * avg(move) / avg(range)` and the DX
/// `100 * |+DI - -DI| / (+DI + -DI)`, neither division guarded. `[adx, +DI, -DI]`.
struct RangeFirstAdx {
    previous: Option<(f64, f64, f64)>,
    range: Average,
    plus: Average,
    minus: Average,
    adx: Average,
}

impl RangeFirstAdx {
    fn next(&mut self, bar: &Bar) -> [f64; 3] {
        let (up, down, range) = match self.previous.replace((bar.high, bar.low, bar.close)) {
            Some((high, low, close)) => (bar.high - high, low - bar.low, lab_range(bar, close)),
            None => (f64::NAN, f64::NAN, bar.high - bar.low),
        };
        let plus_dm = if up > down && up > 0.0 { up } else { 0.0 };
        let minus_dm = if down > up && down > 0.0 { down } else { 0.0 };
        let range = (self.range)(range);
        let plus = 100.0 * (self.plus)(plus_dm) / range;
        let minus = 100.0 * (self.minus)(minus_dm) / range;
        let dx = 100.0 * (plus - minus).abs() / (plus + minus);
        [(self.adx)(dx), plus, minus]
    }
}

/// Set, `first_bar_range` with `missing_ratios` IS crossing_moving_averages_with_adx_filter's ADX:
/// every line to the rounding of its averages, `NaN` exactly where its lines are; the DI lines
/// start at bar `period - 1` and the ADX at `2 * period - 2`, a bar earlier than unset; the stream
/// equals `vectorize` and survives `reset`. Each option alone moves the registry row.
#[test]
fn adx_first_bar_range_is_the_crossing_filters_adx() {
    for (name, b) in &wave_two_sets() {
        for n in [2usize, 5, 14] {
            let tag = format!("{name} adx({n}) first range, missing ratios");
            let set = Adx::new(n as f64).unwrap().with_first_bar_range(true);
            let set = set.with_missing_ratios(true);
            assert_eq!((set.lookback(), set.lookback_full()), (n - 1, 2 * n - 2), "{tag}");
            let got = checked_stream(&tag, Box::new(set), b);
            let avg = || lab_average(2, n);
            let (range, plus, minus, adx) = (avg(), avg(), avg(), avg());
            let mut lab = RangeFirstAdx { previous: None, range, plus, minus, adx };
            let want: Vec<[f64; 3]> = b.iter().map(|bar| lab.next(bar)).collect();
            for (k, line) in got.iter().enumerate() {
                assert_close(&format!("{tag}: line {k}"), line, &col(&want, k), 1e-12, 1.0);
            }
        }
    }
    let b = hourly_bars();
    let want = registry("adx", &[14.0], &b);
    let range = Adx::new(14.0).unwrap().with_first_bar_range(true);
    let got = checked_stream("first range", Box::new(range), &b);
    assert!(!same(&got[1], &want[1]), "first bar range changed nothing");
    let sets = wave_two_sets();
    let (_, flat) = sets.iter().find(|(name, _)| name == "flat start").unwrap();
    let missing = Adx::new(14.0).unwrap().with_missing_ratios(true);
    let got = checked_stream("missing ratios", Box::new(missing), flat);
    assert!(!same(&got[1], &registry("adx", &[14.0], flat)[1]), "missing ratios changed nothing");
}

/// `supertrend_ai_strategy_adaptive`'s ADX (level 3), its length a parameter: `helpers::Rma`
/// averages of the moves (0 on the first bar) and of the true range (missing there), a DI of 0 while
/// the range average is not positive, a DX of 0 while the DI lines sum to nothing, and the ADX the
/// average of the DX from the first bar. `[adx, +DI, -DI]`.
struct LeadAdx {
    previous: Option<(f64, f64, f64)>,
    range: LabRma,
    plus: LabRma,
    minus: LabRma,
    adx: LabRma,
}

impl LeadAdx {
    fn next(&mut self, bar: &Bar) -> [f64; 3] {
        let (up, down, true_range) = match self.previous.replace((bar.high, bar.low, bar.close)) {
            Some((high, low, close)) => (bar.high - high, low - bar.low, lab_range(bar, close)),
            None => (f64::NAN, f64::NAN, f64::NAN),
        };
        let range = self.range.next(true_range);
        let plus = self.plus.next(if up > down && up > 0.0 { up } else { 0.0 });
        let minus = self.minus.next(if down > up && down > 0.0 { down } else { 0.0 });
        let plus_di = if range > 0.0 { 100.0 * plus / range } else { 0.0 };
        let minus_di = if range > 0.0 { 100.0 * minus / range } else { 0.0 };
        let sum = plus_di + minus_di;
        let dx = if sum > 0.0 { 100.0 * (plus_di - minus_di).abs() / sum } else { 0.0 };
        [self.adx.next(dx), plus_di, minus_di]
    }
}

/// Set with `first_bar_moves`, `dx_from_first_bar` IS supertrend_ai_strategy_adaptive's ADX: to the
/// rounding of its averages from bar `period - 1` (0 there), the DI lines too from bar `period`
/// (the rule reads them 0 before, vike leaves them missing); the stream equals `vectorize`, survives
/// `reset` and `with_adx_period`. Set, the ADX moves and the DI lines do not.
#[test]
fn adx_dx_from_first_bar_is_the_adaptive_supertrends_adx() {
    for (name, b) in &wave_two_sets() {
        for n in [2usize, 5, 14] {
            let tag = format!("{name} adx({n}) dx from the first bar");
            let set = Adx::new(n as f64).unwrap().with_first_bar_moves(true);
            let set = set.with_dx_from_first_bar(true);
            assert_eq!((set.lookback(), set.lookback_full()), (n - 1, n), "{tag}");
            let kept = set.clone().with_adx_period(n as f64).unwrap();
            let got = checked_stream(&tag, Box::new(set), b);
            assert_lines_same(&tag, &checked_stream(&tag, Box::new(kept), b), &got);
            let rma = || LabRma::new(n);
            let (range, plus, minus, adx) = (rma(), rma(), rma(), rma());
            let mut lab = LeadAdx { previous: None, range, plus, minus, adx };
            let want: Vec<[f64; 3]> = b.iter().map(|bar| lab.next(bar)).collect();
            assert_close(&format!("{tag}: adx"), &got[0], &col(&want, 0), 1e-12, 1.0);
            assert_eq!(got[0][n - 1].to_bits(), 0.0f64.to_bits(), "{tag}: the first ADX");
            for k in [1, 2] {
                let (line, lab_line) = (&got[k], col(&want, k));
                assert!(line[..n].iter().all(|v| v.is_nan()), "{tag}: line {k} before its start");
                assert_close(&format!("{tag}: line {k}"), &line[n..], &lab_line[n..], 1e-12, 1.0);
            }
        }
    }
    let b = hourly_bars();
    let unset = Adx::new(14.0).unwrap().with_first_bar_moves(true);
    let want = checked_stream("unset", Box::new(unset.clone()), &b);
    let got = checked_stream("lead", Box::new(unset.with_dx_from_first_bar(true)), &b);
    assert!(!same(&got[0], &want[0]) && same(&got[1], &want[1]), "dx from the first bar");
}

/// The lab's Supertrend that starts down (`helpers::Supertrend`, and
/// `golden_vector_trend_orchestrator_gvto`'s), on the `atr` row's ATR, its length and factor
/// parameters: the band arithmetic with the previous bands read as 0 where missing, the trend down
/// while the last line was the upper band, direction `1` down and `-1` up, `1` until the ATR has a
/// value behind it. `(line, direction)`.
struct DownStart {
    atr: Atr,
    factor: f64,
    previous_atr: f64,
    previous_close: f64,
    upper: f64,
    lower: f64,
    line: f64,
}

impl DownStart {
    fn new(n: f64, factor: f64) -> Self {
        let (atr, nan) = (Atr::new(n).unwrap(), f64::NAN);
        DownStart {
            atr,
            factor,
            previous_atr: nan,
            previous_close: nan,
            upper: nan,
            lower: nan,
            line: nan,
        }
    }

    fn next(&mut self, bar: &Bar) -> (f64, f64) {
        let nz = |x: f64| if x.is_nan() { 0.0 } else { x };
        let atr = self.atr.next(bar);
        let mid = (bar.high + bar.low) / 2.0;
        let (raw_upper, raw_lower) = (mid + self.factor * atr, mid - self.factor * atr);
        let (previous_upper, previous_lower) = (nz(self.upper), nz(self.lower));
        let lower = if raw_lower > previous_lower || self.previous_close < previous_lower {
            raw_lower
        } else {
            previous_lower
        };
        let upper = if raw_upper < previous_upper || self.previous_close > previous_upper {
            raw_upper
        } else {
            previous_upper
        };
        let direction = if self.previous_atr.is_nan() {
            1.0
        } else if self.line == previous_upper {
            if bar.close > upper { -1.0 } else { 1.0 }
        } else if bar.close < lower {
            1.0
        } else {
            -1.0
        };
        self.line = if direction < 0.0 { lower } else { upper };
        (self.upper, self.lower, self.previous_atr, self.previous_close) =
            (upper, lower, atr, bar.close);
        (self.line, direction)
    }
}

/// The lab's Supertrend of a bar a rule makes itself (`helpers::HlcSupertrend`), its length and
/// factor parameters: `DownStart`'s law over a `RestartingRma` of the true range (the first bar's
/// `high - low`), a `NaN` in a `max` making it `NaN`. `(line, direction)` of `(high, low, close)`.
struct HlcDownStart {
    factor: f64,
    atr_of: Average,
    /// The last bar's high and close, ATR, bands and line.
    high: f64,
    close: f64,
    atr: f64,
    up: f64,
    down: f64,
    line: f64,
}

impl HlcDownStart {
    fn new(n: usize, factor: f64) -> Self {
        let (atr_of, nan) = (lab_average(2, n), f64::NAN);
        HlcDownStart {
            factor,
            atr_of,
            high: nan,
            close: nan,
            atr: nan,
            up: nan,
            down: nan,
            line: nan,
        }
    }

    fn next(&mut self, high: f64, low: f64, close: f64) -> (f64, f64) {
        let max = |a: f64, b: f64| if a.is_nan() || b.is_nan() { f64::NAN } else { a.max(b) };
        let nz = |x: f64| if x.is_nan() { 0.0 } else { x };
        let range = if self.high.is_nan() {
            high - low
        } else {
            max(max(high - low, (high - self.close).abs()), (low - self.close).abs())
        };
        let atr = (self.atr_of)(range);
        let mid = (high + low) / 2.0;
        let up = mid + self.factor * atr;
        let up = if up < nz(self.up) || self.close > nz(self.up) { up } else { nz(self.up) };
        let down = mid - self.factor * atr;
        let down =
            if down > nz(self.down) || self.close < nz(self.down) { down } else { nz(self.down) };
        let dir = if self.atr.is_nan() {
            1.0
        } else if self.line == nz(self.up) {
            if close > up { -1.0 } else { 1.0 }
        } else if close < down {
            1.0
        } else {
            -1.0
        };
        let line = if dir < 0.0 { down } else { up };
        (self.high, self.close, self.atr, self.up, self.down, self.line) =
            (high, close, atr, up, down, line);
        (line, dir)
    }
}

/// Bars a rule makes from a series: each opens at the last value (the first at its own), its high
/// and low the larger and the smaller of the two.
fn made_bars(values: &[f64]) -> Vec<Bar> {
    let mut open = values[0];
    let bar = |(i, &x): (usize, &f64)| {
        let o = std::mem::replace(&mut open, x);
        Bar::ohlcv(i as i64 * 60_000, o, o.max(x), o.min(x), x, 1.0)
    };
    values.iter().enumerate().map(bar).collect()
}

/// Set, `down_start` IS the lab's Supertrend that starts down: bit for bit over the `atr` row's ATR
/// (`DownStart`), the line from `lookback` and the direction (its sign turned: vike's `1` is up)
/// from the first bar; over `HlcSupertrend`'s average the direction exactly and the line to the
/// average's rounding; on prices, and on bars made from a series about 0, where the bands read as 0
/// on the first bar with an ATR decide. The stream equals `vectorize` and survives `reset`. Unset,
/// the registry row; set, it moves.
#[test]
fn supertrend_down_start_is_the_labs_supertrend() {
    let mut sets = wave_two_sets();
    sets.push(("made about 0".to_string(), made_bars(&closes(0.0, 2.0, 3000))));
    for (name, b) in &sets {
        for (n, factor) in [(10.0, 3.0), (14.0, 2.0), (10.0, 15.0), (1.0, 0.5)] {
            let tag = format!("{name} supertrend({n}, {factor}) down start");
            let set = Supertrend::new(n, factor).unwrap().with_down_start(true);
            let lookback = set.lookback();
            assert_eq!(lookback, n as usize - 1, "{tag}");
            let got = checked_stream(&tag, Box::new(set), b);
            let mut lab = DownStart::new(n, factor);
            let (line, dir): (Vec<f64>, Vec<f64>) = b.iter().map(|bar| lab.next(bar)).unzip();
            let turned: Vec<f64> = dir.iter().map(|d| -d).collect();
            assert!(same(&got[1], &turned), "{tag}: direction");
            assert!(got[0][..lookback].iter().all(|v| v.is_nan()), "{tag}: before the ATR");
            assert!(same(&got[0][lookback..], &line[lookback..]), "{tag}: line");
            let mut hlc = HlcDownStart::new(n as usize, factor);
            let (line, dir): (Vec<f64>, Vec<f64>) =
                b.iter().map(|x| hlc.next(x.high, x.low, x.close)).unzip();
            let turned: Vec<f64> = dir.iter().map(|d| -d).collect();
            assert!(same(&got[1], &turned), "{tag}: direction over the restarting average");
            assert_close(&tag, &got[0][lookback..], &line[lookback..], 1e-12, 1.0);
            let flips = got[1].windows(2).filter(|w| w[0] != w[1]).count();
            assert!(name != "walk at 100" || factor > 10.0 || flips > 20, "{tag}: {flips} flips");
        }
    }
    let b = hourly_bars();
    let want = registry("supertrend", &[10.0, 3.0], &b);
    let off = Supertrend::new(10.0, 3.0).unwrap().with_down_start(false);
    assert_lines_same("supertrend off", &checked_stream("off", Box::new(off), &b), &want);
    let set = Supertrend::new(10.0, 3.0).unwrap().with_down_start(true);
    assert!(!same(&checked_stream("set", Box::new(set), &b)[0], &want[0]), "changed nothing");
}

/// Set, `alpha_steps` IS `helpers::AlphaRma` bit for bit, at three price levels, over closes and
/// (with `skip_nan`) over a true range missing on the first bar, as
/// `buy_sell_strategy_depends_on_ao_stoch_rsi_atr_by` feeds it; the stream equals
/// `vectorize` and survives `reset`. Unset, the registry row; set, it moves.
#[test]
fn smma_alpha_steps_is_the_labs_alpha_rma() {
    for (level, step) in [(100.0, 2.0), (1.0e6, 0.01), (1.0e-3, 1.0e-6)] {
        let b = bodied_bars(&closes(level, step, 3000));
        let mut last = f64::NAN;
        let mut ranges: Vec<f64> =
            b.iter().map(|x| lab_range(x, std::mem::replace(&mut last, x.close))).collect();
        ranges[0] = f64::NAN;
        for n in [2usize, 3, 5, 7, 12, 14] {
            let tag = format!("smma({n}) alpha steps at {level}");
            let set = Smma::new(n as f64).unwrap().with_alpha_steps(true);
            let got = checked_stream(&tag, Box::new(set.clone()), &b).remove(0);
            let mut lab = lab_average(1, n);
            let want: Vec<f64> = b.iter().map(|x| lab(x.close)).collect();
            assert!(same(&got, &want), "{tag}: closes");
            let mut skipping = set.with_skip_nan(true);
            let got: Vec<f64> = ranges.iter().map(|&x| skipping.next_value(x)).collect();
            let mut lab = lab_average(1, n);
            let want: Vec<f64> = ranges.iter().map(|&x| lab(x)).collect();
            assert!(same(&got, &want), "{tag}: ranges");
        }
    }
    let b = hourly_bars();
    let want = registry("smma", &[3.0], &b);
    let off = Smma::new(3.0).unwrap().with_alpha_steps(false);
    assert_lines_same("smma off", &checked_stream("smma off", Box::new(off), &b), &want);
    let set = Smma::new(3.0).unwrap().with_alpha_steps(true);
    assert!(!same(&checked_stream("set", Box::new(set), &b)[0], &want[0]), "changed nothing");
}

/// `rsi_divergence_strategy`'s RSI of `c`, its length a parameter: `AlphaRma`s
/// of the gains and losses (none on the first bar), 100 while the mean loss is 0, else 0 while the
/// mean gain is, else `100 - 100 / (1 + up / down)`.
fn zero_rule_rsi(c: &[f64], n: usize) -> Vec<f64> {
    let (mut gains, mut losses, mut previous) = (lab_average(1, n), lab_average(1, n), f64::NAN);
    c.iter()
        .map(|&x| {
            let change = x - std::mem::replace(&mut previous, x);
            let (gain, loss) = if change.is_nan() {
                (f64::NAN, f64::NAN)
            } else {
                (change.max(0.0), -change.min(0.0))
            };
            let (up, down) = (gains(gain), losses(loss));
            if down == 0.0 {
                100.0
            } else if up == 0.0 {
                0.0
            } else {
                100.0 - 100.0 / (1.0 + up / down)
            }
        })
        .collect()
}

/// Set with `rs_form`, `zero_saturation` IS the RSI of `rsi_divergence_strategy`
/// bit for bit, at three price levels and from a flat start (both means 0: 100, where the RS form
/// alone reads `NaN`); the stream equals `vectorize` and survives `reset`. Unset, the registry row;
/// on the default form, set, nothing moves.
#[test]
fn rsi_zero_saturation_is_the_zero_rule_rsi() {
    for (level, step) in [(100.0, 2.0), (1.0e6, 0.01), (1.0e-3, 1.0e-6)] {
        let mut c = vec![level; 30];
        c.extend(closes(level, step, 3000));
        let b = close_bars(&c);
        for n in [1usize, 5, 12, 14] {
            let tag = format!("rsi({n}) zero saturation at {level}");
            let set = Rsi::new(n as f64).unwrap().with_rs_form(true).with_zero_saturation(true);
            let got = checked_stream(&tag, Box::new(set), &b).remove(0);
            assert!(same(&got, &zero_rule_rsi(&c, n)), "{tag}");
            assert_eq!(got[n], 100.0, "{tag}: a flat start");
            let rs = Rsi::new(n as f64).unwrap().with_rs_form(true);
            assert!(checked_stream(&tag, Box::new(rs), &b)[0][n].is_nan(), "{tag}: unset");
        }
    }
    let b = hourly_bars();
    let want = registry("rsi", &[14.0], &b);
    for zero in [false, true] {
        let plain = Rsi::new(14.0).unwrap().with_zero_saturation(zero);
        assert_lines_same("rsi plain", &checked_stream("rsi plain", Box::new(plain), &b), &want);
    }
}

// ------------------------- smma and rsi from 0, the lab's other Wilder means (level 3, wave I5)

/// `vwma_with_knn_machine_learning_mfi_adx_by_lastgu`'s `ZeroRma` and `s_888_bot_backtest`'s
/// `Wima` over `x`: `((n - 1) * previous + x) / n` from 0, a `NaN` last read as 0; with `alpha`,
/// `pmax_on_rsi_w_t3_strategy`'s means, `alpha * x + (1 - alpha) * previous` from 0.
fn sm_zero_rma(x: &[f64], n: usize, alpha: bool) -> Vec<f64> {
    let (n, mut last) = (n as f64, f64::NAN);
    x.iter()
        .map(|&v| {
            let previous = if last.is_nan() { 0.0 } else { last };
            last = if alpha {
                let a = 1.0 / n;
                a * v + (1.0 - a) * previous
            } else {
                ((n - 1.0) * previous + v) / n
            };
            last
        })
        .collect()
}

/// Set, `zero_seed` IS the lab's Wilder average from 0: to the rounding of its division (the option
/// steps by the row's `(n - 1) / n` and `1 / n`), `NaN` exactly where its is and from 0 again after
/// one; with `alpha_steps`, pmax's means bit for bit; with `skip_nan`, a `NaN` moves nothing; no
/// warm-up, and `seed_first` reads nothing. The stream equals `vectorize` and survives `reset`.
/// Unset, the registry row; set, it moves.
#[test]
fn sm_smma_zero_seed_is_the_labs_average_from_zero() {
    for (name, b) in &wave_two_sets() {
        let c: Vec<f64> = b.iter().map(|x| x.close).collect();
        let mut gaps = c.clone();
        for i in [0, 1, 300, 301, 900] {
            gaps[i] = f64::NAN;
        }
        for n in [2usize, 6, 13, 34] {
            let tag = format!("{name} smma({n}) zero seed");
            let set = Smma::new(n as f64).unwrap().with_zero_seed(true);
            assert_eq!(set.lookback(), 0, "{tag}");
            let got = checked_stream(&tag, Box::new(set.clone()), b).remove(0);
            assert_close(&tag, &got, &sm_zero_rma(&c, n, false), 1e-12, 1.0);
            let first = set.clone().with_seed_first(true);
            assert!(same(&checked_stream(&tag, Box::new(first), b)[0], &got), "{tag}: seed first");
            let mut gapped = set.clone();
            let got: Vec<f64> = gaps.iter().map(|&x| gapped.next_value(x)).collect();
            let want = sm_zero_rma(&gaps, n, false);
            assert_close(&format!("{tag}, gaps"), &got, &want, 1e-12, 1.0);
            let alpha = set.clone().with_alpha_steps(true);
            let got = checked_stream(&tag, Box::new(alpha), b).remove(0);
            assert!(same(&got, &sm_zero_rma(&c, n, true)), "{tag}: alpha steps");
            let mut skipping = set.with_skip_nan(true);
            let got: Vec<f64> = gaps.iter().map(|&x| skipping.next_value(x)).collect();
            let kept: Vec<f64> = gaps.iter().copied().filter(|x| !x.is_nan()).collect();
            let mut kept = sm_zero_rma(&kept, n, false).into_iter();
            let nan_or = |x: &f64| if x.is_nan() { f64::NAN } else { kept.next().unwrap() };
            let want: Vec<f64> = gaps.iter().map(nan_or).collect();
            assert_close(&format!("{tag}, skipping"), &got, &want, 1e-12, 1.0);
        }
    }
    let b = hourly_bars();
    let want = registry("smma", &[13.0], &b);
    let off = Smma::new(13.0).unwrap().with_zero_seed(false);
    assert_lines_same("smma off", &checked_stream("smma off", Box::new(off), &b), &want);
    let set = Smma::new(13.0).unwrap().with_zero_seed(true);
    assert!(!same(&checked_stream("set", Box::new(set), &b)[0], &want[0]), "changed nothing");
}

/// `pmax_on_rsi_w_t3_strategy`'s RSI of `c`, its length a parameter: means of the gains and losses
/// from 0 (the first bar's 0, a comparison with no last close being false), each
/// `alpha * x + (1 - alpha) * previous`, and `100 - 100 / (1 + up / down)` (its guard of a ratio of -1
/// reads nothing: no mean is negative).
fn sm_pmax_rsi(c: &[f64], n: usize) -> Vec<f64> {
    let (alpha, mut last, mut up, mut down) = (1.0 / n as f64, f64::NAN, f64::NAN, f64::NAN);
    let nz = |x: f64| if x.is_nan() { 0.0 } else { x };
    c.iter()
        .map(|&close| {
            let gain = if close >= last { close - last } else { 0.0 };
            let loss = if close < last { last - close } else { 0.0 };
            last = close;
            up = alpha * gain + (1.0 - alpha) * nz(up);
            down = alpha * loss + (1.0 - alpha) * nz(down);
            let rs = up / down;
            if rs == -1.0 { 0.0 } else { 100.0 - 100.0 / (1.0 + rs) }
        })
        .collect()
}

/// The registry row's arithmetic from 0 (no seed): each mean `mean * (n - 1) / n + move / n` from
/// the first move, `100 * up / (up + down)`, 100 while the mean loss is 0.
fn sm_wilder_rsi_from_zero(c: &[f64], n: usize) -> Vec<f64> {
    let (inverse, keep) = (1.0 / n as f64, (n as f64 - 1.0) / n as f64);
    let (mut ag, mut al) = (0.0, 0.0);
    (0..c.len())
        .map(|i| {
            if i == 0 {
                return f64::NAN;
            }
            let d = c[i] - c[i - 1];
            (ag, al) = (ag * keep + d.max(0.0) * inverse, al * keep + (-d).max(0.0) * inverse);
            if al == 0.0 { 100.0 } else { 100.0 * ag / (ag + al) }
        })
        .collect()
}

/// Set with `rs_form`, `zero_seed` IS pmax's RSI bit for bit, at three price levels and from a flat
/// start (both means 0: `NaN`, as the rule's); in the default form it is the row's arithmetic from 0
/// bit for bit. The RSI is there from bar 1; the stream equals `vectorize` and survives `reset`.
/// Unset, the registry row; set, in either form, it moves.
#[test]
fn sm_rsi_zero_seed_is_pmaxs_rsi() {
    for (level, step) in [(100.0, 2.0), (1.0e6, 0.01), (1.0e-3, 1.0e-6)] {
        let mut c = vec![level; 30];
        c.extend(closes(level, step, 3000));
        let b = close_bars(&c);
        for n in [1usize, 5, 14, 40] {
            let tag = format!("rsi({n}) zero seed at {level}");
            let set = Rsi::new(n as f64).unwrap().with_zero_seed(true);
            assert_eq!(set.lookback(), 1, "{tag}");
            let rs = set.clone().with_rs_form(true);
            let got = checked_stream(&tag, Box::new(rs), &b).remove(0);
            assert!(same(&got, &sm_pmax_rsi(&c, n)), "{tag}: the RS form");
            assert!(got[0].is_nan() && got[1].is_nan(), "{tag}: a flat start");
            let got = checked_stream(&tag, Box::new(set), &b).remove(0);
            assert!(same(&got, &sm_wilder_rsi_from_zero(&c, n)), "{tag}: the default form");
        }
    }
    let b = hourly_bars();
    let want = registry("rsi", &[14.0], &b);
    let off = Rsi::new(14.0).unwrap().with_zero_seed(false);
    assert_lines_same("rsi off", &checked_stream("rsi off", Box::new(off), &b), &want);
    for rs_form in [false, true] {
        let set = Rsi::new(14.0).unwrap().with_rs_form(rs_form);
        let unset = checked_stream("unset", Box::new(set.clone()), &b).remove(0);
        let got = checked_stream("set", Box::new(set.with_zero_seed(true)), &b).remove(0);
        assert!(!same(&got, &unset), "zero seed changed nothing, rs form {rs_form}");
    }
}

/// A cRSI's RSI of `c` over the lab's `helpers::Rma` (`LabRma`):
/// the gains and losses missing on the first bar, a mean within 1e-10 of 0 read as none (the
/// losses' first: 100, then the gains': 0), else `100 - 100 / (1 + up / down)`.
fn sm_crsi_rsi(c: &[f64], n: usize) -> Vec<f64> {
    let (mut up, mut down, mut last) = (LabRma::new(n), LabRma::new(n), f64::NAN);
    let none = |a: f64| a == 0.0 || a.abs() <= 1e-10;
    c.iter()
        .map(|&x| {
            let change = x - std::mem::replace(&mut last, x);
            let (gain, loss) = if change.is_nan() {
                (f64::NAN, f64::NAN)
            } else {
                (change.max(0.0), -change.min(0.0))
            };
            let (u, d) = (up.next(gain), down.next(loss));
            if none(d) {
                100.0
            } else if none(u) {
                0.0
            } else {
                100.0 - 100.0 / (1.0 + u / d)
            }
        })
        .collect()
}

/// The lab's `helpers::Rma` (`LabRma`) is `Smma` skipping a `NaN`, to the rounding of its seed (a
/// compensated sum against a left fold) and of its steps (a division against the row's constants),
/// `NaN` exactly where its is: over closes, and over a range missing on its first 20 bars as
/// `ma_candles_supertrend_strategy`'s. And crsi's RSI over it is `Rsi` in the RS form, saturated, to
/// the same rounding (alpha steps there), from a flat start too.
#[test]
fn sm_the_labs_rma_is_smma_and_its_rsi_is_rsi() {
    for (level, step) in [(100.0, 2.0), (1.0e6, 0.01), (1.0e-3, 1.0e-6)] {
        let mut c = vec![level; 40];
        c.extend(closes(level, step, 3000));
        let b = bodied_bars(&c);
        let mut range: Vec<f64> = b.iter().map(|x| x.high - x.low).collect();
        range[..20].fill(f64::NAN);
        for n in [2usize, 5, 20, 30] {
            let tag = format!("smma({n}) skipping at {level}");
            let set = Smma::new(n as f64).unwrap().with_skip_nan(true);
            let got = checked_stream(&tag, Box::new(set.clone()), &b).remove(0);
            let mut lab = LabRma::new(n);
            let want: Vec<f64> = c.iter().map(|&x| lab.next(x)).collect();
            assert_close(&format!("{tag}: closes"), &got, &want, 1e-12, 1.0);
            let (mut set, mut lab) = (set, LabRma::new(n));
            let got: Vec<f64> = range.iter().map(|&x| set.next_value(x)).collect();
            let want: Vec<f64> = range.iter().map(|&x| lab.next(x)).collect();
            assert_close(&format!("{tag}: ranges"), &got, &want, 1e-12, 1.0);
            let rsi = Rsi::new(n as f64).unwrap().with_rs_form(true).with_saturation(true);
            let got = checked_stream(&tag, Box::new(rsi), &b).remove(0);
            assert_close(&format!("rsi({n}) at {level}"), &got, &sm_crsi_rsi(&c, n), 1e-12, 1.0);
            assert_eq!(got[n], 100.0, "rsi({n}) at {level}: a flat start");
        }
    }
}

/// `supertrend_regime_confluence`'s running mean: the mean of the first `length` numbers (their
/// compensated sum, `NaN` skipped), then `alpha * x + (1 - alpha) * mean`.
struct SmRunningMean {
    length: usize,
    seed: RollingSum,
    mean: f64,
}

impl SmRunningMean {
    fn next(&mut self, x: f64) -> f64 {
        self.mean = if self.mean.is_nan() {
            self.seed.next(x) / self.length as f64
        } else {
            let alpha = 1.0 / self.length as f64;
            alpha * x + (1.0 - alpha) * self.mean
        };
        self.mean
    }
}

/// `supertrend_regime_confluence`'s ADX of `b` (level 3), its length a parameter: running means of
/// the moves (0 on the first bar) and of the true range (missing there), a DI of 0 while the range's
/// mean is not positive, a DX of 0 while the DI lines sum to nothing, and the ADX the running mean of
/// the DX from the first bar.
fn sm_regime_adx(b: &[Bar], n: usize) -> Vec<f64> {
    let mean = || SmRunningMean { length: n, seed: RollingSum::new(n), mean: f64::NAN };
    let (mut range, mut plus, mut minus, mut adx) = (mean(), mean(), mean(), mean());
    let mut previous: Option<&Bar> = None;
    b.iter()
        .map(|bar| {
            let none = (f64::NAN, f64::NAN, f64::NAN);
            let (ph, pl, pc) = previous.replace(bar).map_or(none, |p| (p.high, p.low, p.close));
            let (up, down) = (bar.high - ph, pl - bar.low);
            let plus_dm = if up > down && up > 0.0 { up } else { 0.0 };
            let minus_dm = if down > up && down > 0.0 { down } else { 0.0 };
            let tr = range.next(if pc.is_nan() { f64::NAN } else { lab_range(bar, pc) });
            let (p, m) = (plus.next(plus_dm), minus.next(minus_dm));
            let plus_di = if tr > 0.0 { 100.0 * p / tr } else { 0.0 };
            let minus_di = if tr > 0.0 { 100.0 * m / tr } else { 0.0 };
            let sum = plus_di + minus_di;
            adx.next(if sum > 0.0 { 100.0 * (plus_di - minus_di).abs() / sum } else { 0.0 })
        })
        .collect()
}

/// Set with `first_bar_moves`, `dx_from_first_bar` IS supertrend_regime_confluence's ADX: to the
/// rounding of its running means (Wilder's sums here), `NaN` exactly where its is (before bar
/// `period - 1`).
#[test]
fn sm_adx_dx_from_first_bar_is_the_regime_confluences_adx() {
    for (name, b) in &wave_two_sets() {
        for n in [2usize, 5, 14] {
            let tag = format!("{name} adx({n}) of the regime");
            let set = Adx::new(n as f64).unwrap().with_first_bar_moves(true);
            let got = checked_stream(&tag, Box::new(set.with_dx_from_first_bar(true)), b);
            assert_close(&tag, &got[0], &sm_regime_adx(b, n), 1e-12, 1.0);
        }
    }
}

// ----------------------- atr_trailing_stop, true_range, chop, confirmed_pivot: the lab's stops (I5)

use vike_indicators::indicators::{AtrTrailingStop, Chop, TrueRange};

/// The reference's `max` (`high`) or `min`: `NaN` when either is. On numbers it is `f64::max` and
/// `f64::min`, which `ut_bot_v2_atr_trailing_stop` and `ticktock_ut_entry_signal_stop` call.
fn timestamp_extreme(a: f64, b: f64, high: bool) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if high {
        a.max(b)
    } else {
        a.min(b)
    }
}

/// The lab's UT Bot trail over vike's `Atr(period)`, `mult * ATR` from the close: two closes past
/// the last stop on one side ratchet it there, else it jumps to the close's side. `start` is how the
/// rule reads the stop it has not yet got: 0 as 0 (`nz`: `atr_trailing_stop_strategy_by_ceyhun`,
/// `ut_bot_strategy_vynmpw`, `moving_average_displaced_envelope_atrts`, myth-busting #1's `Ut`); 1
/// as missing, so every comparison with it is false (`ut_bot_v2_atr_trailing_stop`); 2 as the close
/// (`ticktock_ut_entry_signal_stop`).
fn timestamp_lab_trail(b: &[Bar], period: f64, mult: f64, start: usize) -> Vec<f64> {
    let mut atr = Atr::new(period).unwrap();
    let (mut stop, mut last_close) = (f64::NAN, f64::NAN);
    b.iter()
        .map(|bar| {
            let loss = mult * atr.next(bar);
            let src = bar.close;
            let previous = match start {
                0 if stop.is_nan() => 0.0,
                2 if stop.is_nan() => src,
                _ => stop,
            };
            stop = if src > previous && last_close > previous {
                timestamp_extreme(previous, src - loss, true)
            } else if src < previous && last_close < previous {
                timestamp_extreme(previous, src + loss, false)
            } else if src > previous {
                src - loss
            } else {
                src + loss
            };
            last_close = src;
            stop
        })
        .collect()
}

/// Unset, `atr_trailing_stop` IS the trail that reads a missing stop as 0 bit for bit (its first
/// value `max(0, close - loss)`, here always `close - loss`); set, `down_start` IS the trail that
/// reads it as missing or as the close bit for bit: the first value `close + loss` at
/// `period - 1`, the same rule after. The stream equals `vectorize` and survives `reset`; unset is the registry
/// row; set, it moves.
#[test]
fn timestamp_atr_trailing_stop_down_start_is_the_labs_missing_stop_trail() {
    let cases = [(1.0, 2.0), (5.0, 0.5), (6.0, 2.0), (10.0, 1.0), (10.0, 3.0), (15.0, 2.0)];
    for (name, b) in &wave_two_sets() {
        for (period, mult) in cases {
            let tag = format!("{name} atr_trailing_stop({period}, {mult})");
            let plain = AtrTrailingStop::new(period, mult).unwrap();
            let plain = checked_stream(&tag, Box::new(plain), b).remove(0);
            let row = registry("atr_trailing_stop", &[period, mult], b).remove(0);
            assert!(same(&plain, &row), "{tag}: unset, the registry row");
            assert!(
                same(&plain, &timestamp_lab_trail(b, period, mult, 0)),
                "{tag}: unset, the nz trail"
            );
            let off = AtrTrailingStop::new(period, mult).unwrap().with_down_start(false);
            assert!(same(&checked_stream(&tag, Box::new(off), b)[0], &plain), "{tag}: off");
            let set = AtrTrailingStop::new(period, mult).unwrap().with_down_start(true);
            let first = period as usize - 1;
            assert_eq!(set.lookback(), first, "{tag}: lookback");
            let got = checked_stream(&tag, Box::new(set), b).remove(0);
            for start in [1, 2] {
                let want = timestamp_lab_trail(b, period, mult, start);
                assert!(same(&got, &want), "{tag}: start {start}");
            }
            assert!(got[..first].iter().all(|v| v.is_nan()) && got[first] >= b[first].close);
        }
    }
    let b = hourly_bars();
    for (period, mult) in cases {
        let set = AtrTrailingStop::new(period, mult).unwrap().with_down_start(true);
        let got = checked_stream("down start", Box::new(set), &b);
        let row = registry("atr_trailing_stop", &[period, mult], &b);
        assert!(!same(&got[0], &row[0]), "({period}, {mult}): down_start moved nothing");
    }
}

/// The lab's true range: `NaN` without a last close where `missing_first` (`alphatrend`,
/// `alphatrend_strategy`, `fractal_strat_kl`, `keltner_channel_eth_usdt_1h`,
/// `profit_maximizer_strategy`, the DMI of `chop_zone_entry_strategy_dmi_psar_exit`,
/// `buy_sell_strategy_depends_on_ao_stoch_rsi_atr_by`), else `f64::max` dropping the missing
/// close, `high - low` (`abcd_strategy`, `orb_strategy_lucitech`,
/// `turtle_trading_strategy_donchian_atr`).
fn timestamp_lab_true_range(b: &[Bar], missing_first: bool) -> Vec<f64> {
    let mut pc = f64::NAN;
    b.iter()
        .map(|bar| {
            let tr = if missing_first && pc.is_nan() { f64::NAN } else { lab_range(bar, pc) };
            pc = bar.close;
            tr
        })
        .collect()
}

/// Unset, `true_range` IS the lab's range with `high - low` on the first bar, bit for bit; set,
/// `first_bar_missing` IS the lab's range that has none without a last close (the first bar, and
/// the bar after a missing close), bit for bit, the same number on every other bar. The stream
/// equals `vectorize` and survives `reset`; unset is the registry row; set, it moves.
#[test]
fn timestamp_true_range_first_bar_missing_is_the_labs_range_without_a_last_close() {
    let mut sets = wave_two_sets();
    let mut gap = hourly_bars();
    gap[300].close = f64::NAN;
    sets.push(("a missing close".to_string(), gap));
    for (name, b) in &sets {
        let plain = checked_stream(name, Box::new(TrueRange::default()), b).remove(0);
        assert!(same(&plain, &registry("true_range", &[], b)[0]), "{name}: the registry row");
        assert!(same(&plain, &timestamp_lab_true_range(b, false)), "{name}: unset, the lab's");
        let off = TrueRange::default().with_first_bar_missing(false);
        assert!(same(&checked_stream(name, Box::new(off), b)[0], &plain), "{name}: off");
        let set = TrueRange::default().with_first_bar_missing(true);
        assert_eq!(set.lookback(), 1, "{name}: lookback");
        let got = checked_stream(name, Box::new(set), b).remove(0);
        assert!(same(&got, &timestamp_lab_true_range(b, true)), "{name}: set, the lab's");
        assert!(got[0].is_nan() && plain[0].is_finite(), "{name}: the first bar moved");
        let moved = (1..b.len()).filter(|&i| bits(got[i]) != bits(plain[i])).count();
        let gaps = usize::from(name == "a missing close");
        assert_eq!(moved, gaps, "{name}: only a bar with no last close moved");
    }
}

/// `chop_zone_entry_strategy_dmi_psar_exit`'s choppiness index, its length a parameter:
/// `100 * log10(sum / range) / log10(n)`, the sum the lab's compensated one of the true range
/// (`high - low` on the first bar), the range of `Donchian(n)` over each bar's
/// `high >= last close ? high : last close` and the mirror (both missing on the first bar, which
/// `Donchian` skips).
fn timestamp_lab_chop(b: &[Bar], n: usize) -> Vec<f64> {
    let mut sum = RollingSum::new(n);
    let mut extremes = Donchian::new(n as f64).unwrap();
    let mut pc = f64::NAN;
    b.iter()
        .map(|bar| {
            let range = sum.next(lab_range(bar, pc));
            let top = if bar.high >= pc { bar.high } else { pc };
            let bottom = if bar.low <= pc { bar.low } else { pc };
            pc = bar.close;
            let [highest, _, lowest] = extremes.next_hl(top, bottom);
            100.0 * libm::log10(range / (highest - lowest)) / libm::log10(n as f64)
        })
        .collect()
}

/// Set, `true_extremes` IS `chop_zone_entry_strategy_dmi_psar_exit`'s index to 1e-12 (vike's
/// running sum and `100 / log10(n)` factor against the lab's compensated sum and division), `NaN`
/// exactly where the lab's is (the warm-up, and a flat window's 0 / 0). Over bars that never stay
/// flat inside the series: there the lab's compensated sum can keep a residue that it divides by a
/// range of 0. The stream equals `vectorize` and survives `reset`; unset is the registry row; set,
/// it moves (on the hourly bars, whose closes gap past the next bar's range).
#[test]
fn timestamp_chop_true_extremes_is_the_chop_zones_index() {
    let sets: Vec<(String, Vec<Bar>)> =
        wave_two_sets().into_iter().filter(|(name, _)| name != "awkward").collect();
    for (name, b) in &sets {
        for n in [2usize, 5, 14, 30] {
            let tag = format!("{name} chop({n}) true extremes");
            let set = Chop::new(n as f64).unwrap().with_true_extremes(true);
            assert_eq!(set.lookback(), n - 1, "{tag}: lookback");
            let got = checked_stream(&tag, Box::new(set), b).remove(0);
            assert_close(&tag, &got, &timestamp_lab_chop(b, n), 1e-12, 1.0);
            assert!(got[n - 1].is_finite() || name == "flat start", "{tag}: the first value");
            let plain = Chop::new(n as f64).unwrap();
            let plain = checked_stream(&tag, Box::new(plain), b);
            assert_lines_same(&format!("{tag}: unset"), &plain, &registry("chop", &[n as f64], b));
            let off = Chop::new(n as f64).unwrap().with_true_extremes(false);
            let off = checked_stream(&tag, Box::new(off), b);
            assert_lines_same(&format!("{tag}: off"), &off, &plain);
            if name == "hourly" {
                assert!(!same(&got, &plain[0]), "{tag}: true extremes moved nothing");
            }
        }
    }
}

/// `lanz_strategy_4_0_backtest`'s `Pivots` (`swing` bars a side), written window by window: the
/// middle bar's high when no older high is above it and every newer one is under it, its low the
/// mirror.
fn timestamp_lab_pivots(b: &[Bar], swing: usize) -> (Vec<f64>, Vec<f64>) {
    let mut bars = std::collections::VecDeque::new();
    b.iter()
        .map(|bar| {
            bars.push_back((bar.high, bar.low));
            if bars.len() > 2 * swing + 1 {
                bars.pop_front();
            }
            if bars.len() < 2 * swing + 1 {
                return (f64::NAN, f64::NAN);
            }
            let (top, bottom) = bars[swing];
            let older = || bars.range(..swing);
            let newer = || bars.range(swing + 1..);
            let is_high = older().all(|x| x.0 <= top) && newer().all(|x| x.0 < top);
            let is_low = older().all(|x| x.1 >= bottom) && newer().all(|x| x.1 > bottom);
            (if is_high { top } else { f64::NAN }, if is_low { bottom } else { f64::NAN })
        })
        .unzip()
}

/// `rsi_divergence_indicator`'s `S::pivot` over a value, `left` and `right` its constants made
/// parameters: the value `right` back when it beats every newer one strictly and no older one
/// beats it, none with a missing value among the older ones or at the candidate.
fn timestamp_lab_value_pivot(x: &[f64], left: usize, right: usize, low: bool) -> Vec<f64> {
    let beats = |a: f64, b: f64| if low { a < b } else { a > b };
    let mut window = std::collections::VecDeque::new();
    x.iter()
        .map(|&v| {
            window.push_front(v);
            window.truncate(left + 1 + right);
            if window.len() < left + 1 + right {
                return f64::NAN;
            }
            let p = window[right];
            let newer = window.iter().take(right).all(|&w| beats(p, w));
            let older = window.iter().skip(right + 1).all(|&w| !beats(w, p) && !w.is_nan());
            if !p.is_nan() && newer && older { p } else { f64::NAN }
        })
        .collect()
}

/// `confirmed_pivot`'s `left` and `right` reach 200: `ConfirmedPivot::new(180, 180)` IS lanz's
/// pivots bit for bit, at 101 and 200 too, with the slice call and the registry row the same; one
/// past the end is refused. And `rsi_divergence_indicator`'s pivot of RSI(9), one bar to the left
/// and three to the right, IS `ConfirmedPivot::new(1, 3)` fed the RSI as high and low, bit for bit
/// from its missing start (with one bar to the left the only older value is the window's oldest,
/// where both rules confirm nothing on a missing value).
#[test]
fn timestamp_confirmed_pivot_to_200_is_lanzs_pivots() {
    let walk = bodied_bars(&closes(100.0, 2.0, 6000));
    for (name, b) in [("hourly", hourly_bars()), ("walk", walk)] {
        for swing in [101usize, 180, 200] {
            let tag = format!("{name} confirmed_pivot({swing}, {swing})");
            let p = ConfirmedPivot::new(swing as f64, swing as f64).unwrap();
            let got = checked_stream(&tag, Box::new(p), &b);
            let (highs, lows) = timestamp_lab_pivots(&b, swing);
            assert!(same(&got[0], &highs) && same(&got[1], &lows), "{tag}: the pivots");
            let found = highs.iter().chain(&lows).filter(|v| !v.is_nan()).count();
            assert!(found >= 2 || name == "hourly", "{tag}: only {found} pivots");
            let (h, l): (Vec<f64>, Vec<f64>) = b.iter().map(|x| (x.high, x.low)).unzip();
            let slice: Vec<Vec<f64>> =
                series::confirmed_pivot(&h, &l, swing as f64, swing as f64).unwrap().into();
            assert_lines_same(&format!("{tag}: series"), &slice, &got);
            let row = registry("confirmed_pivot", &[swing as f64, swing as f64], &b);
            assert_lines_same(&format!("{tag}: registry"), &row, &got);
        }
    }
    assert!(ConfirmedPivot::new(201.0, 5.0).is_err() && ConfirmedPivot::new(5.0, 201.0).is_err());
    let b = hourly_bars();
    let mut rsi = Rsi::new(9.0).unwrap();
    let x: Vec<f64> = b.iter().map(|bar| rsi.next(bar)).collect();
    assert!(x[0].is_nan() && x[20].is_finite());
    let as_bars = close_bars(&x);
    for right in [1usize, 3, 4] {
        let tag = format!("confirmed_pivot(1, {right}) of rsi(9)");
        let p = ConfirmedPivot::new(1.0, right as f64).unwrap();
        let got = checked_stream(&tag, Box::new(p), &as_bars);
        assert!(same(&got[0], &timestamp_lab_value_pivot(&x, 1, right, false)), "{tag}: highs");
        assert!(same(&got[1], &timestamp_lab_value_pivot(&x, 1, right, true)), "{tag}: lows");
        assert!(got[1].iter().filter(|v| !v.is_nan()).count() > 10, "{tag}: lows found");
    }
}

// ------------------------- ema, t3 and kama: the lab's hand-assembled averages (level 3, wave 5)

use vike_indicators::indicators::{Kama, T3};

/// `gradient_trend_filter_strategy_chartprime_pinein`'s stage, its length a parameter: a variable
/// started at 0, then `alpha * x + (1 - alpha) * stage` on every value.
fn ma_zero_smoothing(x: &[f64], n: f64) -> Vec<f64> {
    let alpha = 2.0 / (n + 1.0);
    let mut stage = 0.0;
    x.iter()
        .map(|&v| {
            stage = alpha * v + (1.0 - alpha) * stage;
            stage
        })
        .collect()
}

/// Set, `zero_seed` IS the lab's smoothing started at 0 bit for bit, one stage and three chained
/// (the gradient filter), from the first bar on; the stream equals `vectorize` and survives
/// `reset`. Unset, the registry row; set, it moves.
#[test]
fn ema_zero_seed_is_the_gradient_filters_smoothing() {
    let ema = |n: f64| vike_indicators::indicators::Ema::new(n).unwrap();
    for (name, b) in wave_two_sets() {
        let c: Vec<f64> = b.iter().map(|x| x.close).collect();
        for n in [1.0, 2.0, 25.0, 200.0] {
            let tag = format!("ema({n}) zero seed, {name}");
            let set = ema(n).with_zero_seed(true);
            assert_eq!(set.lookback(), 0, "{tag}: lookback");
            let got = checked_stream(&tag, Box::new(set.clone()), &b).remove(0);
            assert!(same(&got, &ma_zero_smoothing(&c, n)), "{tag}: one stage");
            let mut stages = [set.clone(), set.clone(), set];
            let got: Vec<f64> =
                c.iter().map(|&x| stages.iter_mut().fold(x, |v, s| s.next_value(v))).collect();
            let want = ma_zero_smoothing(&ma_zero_smoothing(&ma_zero_smoothing(&c, n), n), n);
            assert!(same(&got, &want), "{tag}: three chained");
        }
    }
    let b = hourly_bars();
    let want = registry("ema", &[25.0], &b);
    let off = ema(25.0).with_zero_seed(false);
    assert_lines_same("ema off", &checked_stream("ema off", Box::new(off), &b), &want);
    let set = ema(25.0).with_zero_seed(true);
    assert!(!same(&checked_stream("set", Box::new(set), &b)[0], &want[0]), "changed nothing");
}

/// `pmax_on_rsi_w_t3_strategy`'s T3, its length and factor parameters: six vike `Ema`s chained, each
/// skipping a `NaN`, weighted by the coefficients as the rule writes them.
fn ma_six_ema_t3(x: &[f64], n: f64, a: f64) -> Vec<f64> {
    let ema = || vike_indicators::indicators::Ema::new(n).unwrap().with_skip_nan(true);
    let mut emas = [ema(), ema(), ema(), ema(), ema(), ema()];
    x.iter()
        .map(|&v| {
            let mut e = v;
            let mut es = [0.0; 6];
            for (k, ema) in emas.iter_mut().enumerate() {
                e = ema.next_value(e);
                es[k] = e;
            }
            let c1 = -a * a * a;
            let c2 = 3.0 * a * a + 3.0 * a * a * a;
            let c3 = -6.0 * a * a - 3.0 * a - 3.0 * a * a * a;
            let c4 = 1.0 + 3.0 * a + a * a * a + 3.0 * a * a;
            c1 * es[5] + c2 * es[4] + c3 * es[3] + c4 * es[2]
        })
        .collect()
}

/// Set, `six_emas` IS the rule's six-EMA T3 bit for bit, over closes and over a series that starts
/// with 14 `NaN`s and has one more inside (the rule feeds an RSI), with its first value on the
/// registry row's bar; the stream equals `vectorize` and survives `reset`. Unset, the registry row;
/// set, it moves (the seeds differ).
#[test]
fn t3_six_emas_is_the_pmax_rules_t3() {
    for (name, b) in wave_two_sets() {
        let c: Vec<f64> = b.iter().map(|x| x.close).collect();
        let mut gapped = c.clone();
        gapped[..14].fill(f64::NAN);
        gapped[700] = f64::NAN;
        for (n, v) in [(8.0, 0.7), (2.0, 0.7), (20.0, 0.0), (5.0, 1.0), (13.0, 0.35)] {
            let tag = format!("t3({n}, {v}) six emas, {name}");
            let set = T3::new(n, v).unwrap().with_six_emas(true);
            let got = checked_stream(&tag, Box::new(set.clone()), &b).remove(0);
            assert!(same(&got, &ma_six_ema_t3(&c, n, v)), "{tag}: closes");
            let lookback = set.lookback();
            assert!(got[lookback - 1].is_nan() && got[lookback].is_finite(), "{tag}: lookback");
            let mut fed = set;
            let got: Vec<f64> = gapped.iter().map(|&x| fed.next_value(x)).collect();
            assert!(same(&got, &ma_six_ema_t3(&gapped, n, v)), "{tag}: gapped");
        }
    }
    let b = hourly_bars();
    let want = registry("t3", &[8.0, 0.7], &b);
    let off = T3::new(8.0, 0.7).unwrap().with_six_emas(false);
    assert_lines_same("t3 off", &checked_stream("t3 off", Box::new(off), &b), &want);
    let set = T3::new(8.0, 0.7).unwrap().with_six_emas(true);
    assert!(!same(&checked_stream("set", Box::new(set), &b)[0], &want[0]), "changed nothing");
}

/// `kama_strategy_kaufman_s_adaptive_moving_average`'s average, its constants parameters: the
/// `n`-bar move over the lab's compensated sum of the `n` one-bar moves (0 while that sum is 0 or
/// missing), the platforms' smoothing, the average started from 0.
fn ma_zero_kama(c: &[f64], n: usize, fast: f64, slow: f64) -> Vec<f64> {
    let mut closes = std::collections::VecDeque::new();
    let mut moves = RollingSum::new(n);
    let mut kama = f64::NAN;
    c.iter()
        .map(|&close| {
            let last = closes.back().copied().unwrap_or(f64::NAN);
            let volatility = moves.next((close - last).abs());
            closes.push_back(close);
            if closes.len() > n + 1 {
                closes.pop_front();
            }
            let momentum = if closes.len() > n { (close - closes[0]).abs() } else { f64::NAN };
            let ratio =
                if volatility != 0.0 && !volatility.is_nan() { momentum / volatility } else { 0.0 };
            let (fast, slow) = (2.0 / (fast + 1.0), 2.0 / (slow + 1.0));
            let sc = (ratio * (fast - slow) + slow).powi(2);
            let before = if kama.is_nan() { 0.0 } else { kama };
            kama = sc * close + (1.0 - sc) * before;
            kama
        })
        .collect()
}

/// Set with a flat ratio of 0, `zero_seed` IS the lab's KAMA from 0 to 1e-12 from the first bar,
/// with no `NaN` anywhere (vike's path is a running sum where the lab's is compensated, so the
/// ratio's last bits differ); the stream equals `vectorize` and survives `reset`. Unset, the
/// registry row; set, it moves.
#[test]
fn kama_zero_seed_is_the_kama_rules_average() {
    let kama = |n: usize, fast: f64, slow: f64| {
        Kama::new(n as f64, fast, slow).and_then(|k| k.with_flat_ratio(0.0)).unwrap()
    };
    for (name, b) in wave_two_sets() {
        let c: Vec<f64> = b.iter().map(|x| x.close).collect();
        for (n, fast, slow) in [(14, 2.0, 20.0), (10, 2.0, 30.0), (2, 1.0, 5.0), (40, 3.0, 60.0)] {
            let tag = format!("kama({n}, {fast}, {slow}) zero seed, {name}");
            let set = kama(n, fast, slow).with_zero_seed(true);
            assert_eq!(set.lookback(), 0, "{tag}: lookback");
            let got = checked_stream(&tag, Box::new(set), &b).remove(0);
            assert!(got.iter().all(|x| x.is_finite()), "{tag}: a NaN");
            assert_close(&tag, &got, &ma_zero_kama(&c, n, fast, slow), 1e-12, 1e-300);
        }
    }
    let b = hourly_bars();
    let want = registry("kama", &[14.0, 2.0, 20.0], &b);
    let off = Kama::new(14.0, 2.0, 20.0).unwrap().with_zero_seed(false);
    assert_lines_same("kama off", &checked_stream("kama off", Box::new(off), &b), &want);
    let set = Kama::new(14.0, 2.0, 20.0).unwrap().with_zero_seed(true);
    assert!(!same(&checked_stream("set", Box::new(set), &b)[0], &want[0]), "changed nothing");
}

// ----------------------------- stochastic, aroon, zscore: the lab's oscillators (level 3, wave I5)

use vike_indicators::indicators::{Aroon, Stochastic, Zscore};

/// The series without a window of no range: a stochastic's raw `%K` of such a window is 0 in vike
/// and `NaN` in the lab (`0 / 0`), which the lab's sum then skips.
fn os_ranged_sets() -> Vec<(String, Vec<Bar>)> {
    let mut sets = wave_two_sets();
    sets.retain(|(name, _)| name != "awkward" && name != "flat start");
    sets
}

/// Set B's hand stochastic (`combo_backtest_123_reversal_rsi`, `ema_rsi_atr_hidden_div_strat` and
/// six more), its lengths parameters: the raw `100 * (close - lowest low) / (highest high - lowest
/// low)` over the last `n` bars (`NaN` until `n` are in), `%K` the lab's compensated `RollingSum`
/// of `k` raw values over `k`, `%D` the same of `d` values of `%K`; also the raw value.
fn os_lab_stochastic(b: &[Bar], n: usize, k: usize, d: usize) -> [Vec<f64>; 3] {
    let (mut ks, mut d_sum) = (RollingSum::new(k), RollingSum::new(d));
    let mut out = [Vec::new(), Vec::new(), Vec::new()];
    for (i, bar) in b.iter().enumerate() {
        let raw = if i + 1 < n {
            f64::NAN
        } else {
            let w = &b[i + 1 - n..=i];
            let hi = w.iter().map(|x| x.high).fold(f64::NEG_INFINITY, f64::max);
            let lo = w.iter().map(|x| x.low).fold(f64::INFINITY, f64::min);
            100.0 * (bar.close - lo) / (hi - lo)
        };
        let pk = ks.next(raw) / k as f64;
        out[1].push(d_sum.next(pk) / d as f64);
        out[0].push(pk);
        out[2].push(raw);
    }
    out
}

/// Set, `fresh_sum` IS the lab's stochastic: both lines to 1e-12 (fresh folds of the window against
/// the lab's compensated sums), `NaN` exactly where the lab's are, and a smoothing of 1 the raw
/// `%K` bit for bit; a run of equal raw values averages to exactly that value; the stream equals
/// `vectorize` and survives `reset`. Unset, the registry row; set, it moves.
#[test]
fn stochastic_fresh_sum_is_the_labs_stochastic() {
    for (name, b) in &os_ranged_sets() {
        for (n, k, d) in [(14usize, 3usize, 3usize), (14, 1, 3), (3, 4, 3), (10, 3, 1), (5, 3, 1)] {
            let tag = format!("{name} stochastic({n}, {k}, {d}) fresh");
            let set = Stochastic::new(n as f64, k as f64, d as f64).unwrap().with_fresh_sum(true);
            let got = checked_stream(&tag, Box::new(set), b);
            let [lab_k, lab_d, raw] = os_lab_stochastic(b, n, k, d);
            assert_close(&format!("{tag}: %K"), &got[0], &lab_k, 1e-12, 1.0);
            assert_close(&format!("{tag}: %D"), &got[1], &lab_d, 1e-12, 1.0);
            if k == 1 {
                assert!(same(&got[0], &raw), "{tag}: a smoothing of 1 is the raw %K");
            }
            let plain = Stochastic::new(n as f64, k as f64, d as f64).unwrap();
            let row = registry("stochastic", &[n as f64, k as f64, d as f64], b);
            let unset = checked_stream(&tag, Box::new(plain.with_fresh_sum(false)), b);
            assert_lines_same(&tag, &unset, &row);
        }
    }
    let b = bodied_bars(&closes(1.0e6, 0.01, 4000));
    let set = Stochastic::new(14.0, 3.0, 3.0).unwrap().with_fresh_sum(true);
    let row = registry("stochastic", &[14.0, 3.0, 3.0], &b);
    assert!(!same(&checked_stream("set", Box::new(set), &b)[1], &row[1]), "changed nothing");
    // Closing at the high of a rising run: a raw %K of 100 on every bar, both means exactly 100.
    let rising: Vec<Bar> = (0..60_i32)
        .map(|i| {
            let x = 100.0 + 0.37 * f64::from(i);
            Bar::ohlcv(i64::from(i) * 60_000, x - 0.2, x, x - 0.5, x, 1.0)
        })
        .collect();
    let mut fresh = Stochastic::new(5.0, 3.0, 3.0).unwrap().with_fresh_sum(true);
    let mut last = [f64::NAN; 2];
    for bar in &rising {
        last = fresh.next(bar);
    }
    assert_eq!(last, [100.0, 100.0], "equal values average to that value");
}

/// `nononsense_forex`'s Aroon, its length a parameter: over the last `n + 1` bars, oldest first,
/// the first bar strictly past every older one holds the extreme (so a tie goes to the older bar),
/// and each line is `100 * (n - bars_ago) / n`; `NaN` until `n + 1` bars are in.
fn os_older_tie_aroon(b: &[Bar], n: usize) -> [Vec<f64>; 2] {
    let mut out = [vec![f64::NAN; b.len()], vec![f64::NAN; b.len()]];
    let line = |at: usize| {
        let ago = n - at;
        100.0 * (n - ago) as f64 / n as f64
    };
    for (start, w) in b.windows(n + 1).enumerate() {
        let (mut hi, mut lo) = (0, 0);
        for (j, bar) in w.iter().enumerate() {
            if bar.high > w[hi].high {
                hi = j;
            }
            if bar.low < w[lo].low {
                lo = j;
            }
        }
        out[0][start + n] = line(hi);
        out[1][start + n] = line(lo);
    }
    out
}

/// Set, `older_ties` IS the lab's Aroon bit for bit, over series with runs of equal highs and lows
/// (a flat stretch, a flat start, bars cut to whole numbers); the stream equals `vectorize` and
/// survives `reset`. Unset, the registry row (a tie to the newest bar); set, the ties move it.
#[test]
fn aroon_older_ties_is_the_labs_aroon() {
    let mut sets = wave_two_sets();
    let whole: Vec<Bar> = hourly_bars()
        .iter()
        .map(|x| Bar::ohlcv(x.timestamp, x.open, x.high.round(), x.low.round(), x.close, x.volume))
        .collect();
    sets.push(("whole numbers".to_string(), whole));
    for (name, b) in &sets {
        for n in [2usize, 8, 14, 25] {
            let tag = format!("{name} aroon({n}) older ties");
            let set = Aroon::new(n as f64).unwrap().with_older_ties(true);
            let got = checked_stream(&tag, Box::new(set), b);
            let want = os_older_tie_aroon(b, n);
            assert!(same(&got[0], &want[0]) && same(&got[1], &want[1]), "{tag}");
            let row = registry("aroon", &[n as f64], b);
            let plain = Aroon::new(n as f64).unwrap().with_older_ties(false);
            assert_lines_same(&tag, &checked_stream(&tag, Box::new(plain), b), &row);
            if name == "whole numbers" {
                assert!(!same(&got[0], &row[0]) && !same(&got[1], &row[1]), "{tag}: ties");
            }
        }
    }
}

/// `buy_sell_strategy_with_z_score_tradedots`'s z-score, its length a parameter: `p` and `q` the
/// lab's compensated sums of the last `n` closes and of their squares, the deviation
/// `sqrt(max(q / (n - 1) - p * mean / (n - 1), 0))`, the value `(close - mean) / deviation`.
fn os_sample_zscore(c: &[f64], n: usize) -> Vec<f64> {
    let (mut sum, mut squares, nf) = (RollingSum::new(n), RollingSum::new(n), n as f64);
    c.iter()
        .map(|&x| {
            let (p, q) = (sum.next(x), squares.next(x * x));
            let mean = p / nf;
            (x - mean) / (q / (nf - 1.0) - p * mean / (nf - 1.0)).max(0.0).sqrt()
        })
        .collect()
}

/// Set, `sample_deviation` IS the lab's z-score to 1e-12, `NaN` exactly where the lab's is, over
/// noise about 2: the lab's sums of squares lose as many digits as a window's level exceeds its
/// spread, so over a walk, or a window of two close values, the difference is the lab's own error.
/// The stream equals `vectorize` and survives `reset`. Unset, the registry row; set, every value
/// moves.
#[test]
fn zscore_sample_deviation_is_the_labs_zscore() {
    let mut r = crate::fast::XorShift(0x9E37_79B9_7F4A_7C15);
    let c: Vec<f64> = (0..3000).map(|_| 2.0 + 4.0 * (r.unit() - 0.5)).collect();
    let b = close_bars(&c);
    for n in [5usize, 20, 80, 200] {
        let tag = format!("zscore({n}) sample");
        let set = Zscore::new(n as f64).unwrap().with_sample_deviation(true);
        let got = checked_stream(&tag, Box::new(set), &b).remove(0);
        assert_close(&tag, &got, &os_sample_zscore(&c, n), 1e-12, 1.0);
        let row = registry("zscore", &[n as f64], &b);
        let plain = Zscore::new(n as f64).unwrap().with_sample_deviation(false);
        assert_lines_same(&tag, &checked_stream(&tag, Box::new(plain), &b), &row);
        let moved = got.iter().zip(&row[0]).filter(|(g, w)| bits(**g) != bits(**w)).count();
        assert_eq!(moved, c.len() - (n - 1), "{tag}: every value moves");
    }
}

// ------------------------------- vidya from 0: the lab's variable index average (level 3, wave I5)

use vike_indicators::indicators::Vidya;

/// `strategy_lab_b`'s `helpers::Var` (the variable index average of KivancOzbilgic's rules), its
/// lengths parameters: 0 at first, then `nz(k * x) + (1 - k) * value` on every value, `k` being
/// `2 / (length + 1)` times the absolute Chande momentum of the last `cmo` changes, whose two sums
/// are the lab's compensated `RollingSum` (`nz`: 0 until `cmo` are in, or when they sum to
/// nothing); a change from or to a `NaN` is none, and the first value's counts as none.
fn va_lab_var(values: &[f64], length: f64, cmo: usize) -> Vec<f64> {
    let nz = |v: f64| if v.is_nan() { 0.0 } else { v };
    let alpha = 2.0 / (length + 1.0);
    let (mut ups, mut downs) = (RollingSum::new(cmo), RollingSum::new(cmo));
    let (mut previous, mut value) = (f64::NAN, 0.0);
    values
        .iter()
        .map(|&x| {
            let up = if x > previous { x - previous } else { 0.0 };
            let down = if x < previous { previous - x } else { 0.0 };
            previous = x;
            let (u, d) = (ups.next(up), downs.next(down));
            let k = alpha * nz((u - d) / (u + d)).abs();
            value = nz(k * x) + (1.0 - k) * value;
            value
        })
        .collect()
}

/// Set, `zero_seed` IS the lab's variable index average to 1e-12 from the first bar, exactly 0 until
/// `cmo` changes are in, with no `NaN` anywhere (vike's two sums are running sums rebuilt now and then
/// where the lab's are compensated, so the index's last bits can differ): over closes, and over what
/// the stochastic tracker feeds it, a 250-bar raw `%K` (`NaN` for its first 249 bars) with one more
/// `NaN` inside. The stream equals `vectorize` and survives `reset`. Unset, the registry row; set, it
/// moves.
#[test]
fn va_vidya_zero_seed_is_the_labs_variable_index_average() {
    for (name, b) in wave_two_sets() {
        let c: Vec<f64> = b.iter().map(|x| x.close).collect();
        let mut stoch = Stochastic::new(250.0, 1.0, 1.0).unwrap().with_fresh_sum(true);
        let mut k: Vec<f64> = b.iter().map(|bar| stoch.next(bar)[0]).collect();
        k[700] = f64::NAN;
        let rows = [2.0, 3.0, 9.0, 12.0, 26.0, 40.0, 50.0].map(|n| (n, 9usize));
        for (n, cmo) in rows.into_iter().chain([(14.0, 2), (14.0, 30)]) {
            let tag = format!("vidya({n}, {cmo}) zero seed, {name}");
            let set = Vidya::new(n, cmo as f64).unwrap().with_zero_seed(true);
            assert_eq!(set.lookback(), 0, "{tag}: lookback");
            let got = checked_stream(&tag, Box::new(set.clone()), &b).remove(0);
            assert!(got.iter().all(|x| x.is_finite()), "{tag}: a NaN");
            assert!(got[..cmo - 1].iter().all(|&x| x == 0.0), "{tag}: the start");
            assert_close(&tag, &got, &va_lab_var(&c, n, cmo), 1e-12, 1e-300);
            let mut fed = set;
            let got: Vec<f64> = k.iter().map(|&x| fed.next_value(x)).collect();
            assert!(got[..250].iter().all(|&x| x == 0.0), "{tag}: the stochastic's start");
            let want = va_lab_var(&k, n, cmo);
            assert_close(&format!("{tag}, stochastic"), &got, &want, 1e-12, 1.0);
        }
    }
    let b = hourly_bars();
    let want = registry("vidya", &[14.0, 9.0], &b);
    let off = Vidya::new(14.0, 9.0).unwrap().with_zero_seed(false);
    assert_lines_same("vidya off", &checked_stream("vidya off", Box::new(off), &b), &want);
    let set = Vidya::new(14.0, 9.0).unwrap().with_zero_seed(true);
    assert!(!same(&checked_stream("set", Box::new(set), &b)[0], &want[0]), "changed nothing");
}

// ------------------------------- supertrend: held direction, gated turns, carry on close, a given centre (I5)

/// Bars on a small integer grid (opens, closes 0 to 6, highs and lows a step past them or not), where the bands meet and cross
/// often: every number is exact, so bands that meet are equal.
fn st_grid_bars(n: usize) -> Vec<Bar> {
    let mut seed = 0x2545_f491_4f6c_dd1d_u64;
    let mut draw = |k: u64| {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963);
        ((seed >> 33) % k) as f64
    };
    let mut close = 3.0;
    (0..n)
        .map(|i| {
            let open = close;
            close = draw(7);
            let (high, low) = (open.max(close) + draw(2), open.min(close) - draw(2));
            Bar::ohlcv(i as i64 * 60_000, open, high, low, close, 1.0)
        })
        .collect()
}

/// The wave-2 sets, bars made from a series about 0 and the integer grid.
pub(crate) fn st_sets() -> Vec<(String, Vec<Bar>)> {
    let mut sets = wave_two_sets();
    sets.push(("made about 0".to_string(), made_bars(&closes(0.0, 2.0, 3000))));
    sets.push(("grid".to_string(), st_grid_bars(3000)));
    sets
}

/// The `atr` row's ATR of each bar.
pub(crate) fn st_atr(b: &[Bar], n: f64) -> Vec<f64> {
    let mut atr = Atr::new(n).unwrap();
    b.iter().map(|bar| atr.next(bar)).collect()
}

/// The registry form's ATR: Wilder's average of the true ranges from the second bar on (the first has no last close).
pub(crate) fn st_skip_atr(b: &[Bar], n: f64) -> Vec<f64> {
    let mut range = TrueRange::default().with_first_bar_missing(true);
    // Wilder's average of one value is that value (the `smma` row starts at 2).
    let mut mean = (n > 1.0).then(|| Smma::new(n).unwrap().with_skip_nan(true));
    let step = |bar: &Bar| {
        let r = range.next(bar);
        mean.as_mut().map_or(r, |m| m.next_value(r))
    };
    b.iter().map(step).collect()
}

/// `supertrend_strategy`'s ATR: the fresh-sum SMA of the true range (the first bar's `high - low`).
fn st_sma_atr(b: &[Bar], n: f64) -> Vec<f64> {
    let mut range = TrueRange::default();
    let mut mean = Sma::new(n).unwrap().with_fresh_sum(true);
    b.iter().map(|bar| mean.next_value(range.next(bar))).collect()
}

/// A stepped line from the closes, as a pivot line moves: none for the first 15 bars, then every 6th bar a third of the way to
/// the close.
fn st_centre(b: &[Bar]) -> Vec<f64> {
    let mut c = f64::NAN;
    let step = |(i, bar): (usize, &Bar)| {
        if i >= 15 && i % 6 == 0 {
            c = if c.is_nan() { bar.close } else { (c * 2.0 + bar.close) / 3.0 };
        }
        c
    };
    b.iter().enumerate().map(step).collect()
}

/// `strategy_hma_50_supertrend_sniper`'s Supertrend (its `Trend`), length and factor parameters: bands carried from 0, down
/// through the first bar with an ATR, then the trend kept by its own flag. `(line, direction)`, `-1` up and `1` down.
struct StFlagTrend {
    atr: Atr,
    factor: f64,
    upper: f64,
    lower: f64,
    close: f64,
    atr_previous: f64,
    on_upper: bool,
}

impl StFlagTrend {
    fn new(n: f64, factor: f64) -> Self {
        let (atr, nan, on_upper) = (Atr::new(n).unwrap(), f64::NAN, false);
        StFlagTrend { atr, factor, upper: nan, lower: nan, close: nan, atr_previous: nan, on_upper }
    }

    fn next(&mut self, bar: &Bar) -> (f64, f64) {
        let nz = |x: f64| if x.is_nan() { 0.0 } else { x };
        let atr = self.atr.next(bar);
        let mid = (bar.high + bar.low) / 2.0;
        let (upper, lower) = (mid + self.factor * atr, mid - self.factor * atr);
        let (previous_upper, previous_lower) = (nz(self.upper), nz(self.lower));
        self.lower = if lower > previous_lower || self.close < previous_lower {
            lower
        } else {
            previous_lower
        };
        self.upper = if upper < previous_upper || self.close > previous_upper {
            upper
        } else {
            previous_upper
        };
        let direction = if self.atr_previous.is_nan() {
            1.0
        } else if self.on_upper {
            if bar.close > self.upper { -1.0 } else { 1.0 }
        } else if bar.close < self.lower {
            1.0
        } else {
            -1.0
        };
        self.on_upper = direction == 1.0 && !self.upper.is_nan();
        (self.close, self.atr_previous) = (bar.close, atr);
        (if direction < 0.0 { self.lower } else { self.upper }, direction)
    }
}

/// `elliott_wave_with_supertrend_exit_strategy_prese`'s Supertrend (its `LateSupertrend`), length and factor parameters: the
/// first bar read as nothing, the first bar with an ATR down with its own bands, then the trend kept by its flag.
/// `(line, direction)`, `-1` up and `1` down.
struct StLateTrend {
    atr: Atr,
    factor: f64,
    seen: bool,
    started: bool,
    lower: f64,
    upper: f64,
    close: f64,
    on_upper: bool,
    direction: f64,
}

impl StLateTrend {
    fn new(n: f64, factor: f64) -> Self {
        let (atr, nan) = (Atr::new(n).unwrap(), f64::NAN);
        let (seen, started, on_upper) = (false, false, false);
        let (lower, upper, close, direction) = (nan, nan, nan, 1.0);
        StLateTrend { atr, factor, seen, started, lower, upper, close, on_upper, direction }
    }

    fn next(&mut self, bar: &Bar) -> (f64, f64) {
        let atr = self.atr.next(bar);
        if !std::mem::replace(&mut self.seen, true) || atr.is_nan() {
            return (f64::NAN, if self.started { self.direction } else { 1.0 });
        }
        let src = (bar.high + bar.low) / 2.0;
        let (upper, lower) = (src + self.factor * atr, src - self.factor * atr);
        if !self.started {
            self.started = true;
            (self.direction, self.on_upper) = (1.0, true);
            (self.lower, self.upper, self.close) = (lower, upper, bar.close);
            return (upper, 1.0);
        }
        let lower = if lower > self.lower || self.close < self.lower { lower } else { self.lower };
        let upper = if upper < self.upper || self.close > self.upper { upper } else { self.upper };
        self.direction = if self.on_upper {
            if bar.close > upper { -1.0 } else { 1.0 }
        } else if bar.close < lower {
            1.0
        } else {
            -1.0
        };
        self.on_upper = self.direction == 1.0;
        (self.lower, self.upper, self.close) = (lower, upper, bar.close);
        (if self.direction < 0.0 { lower } else { upper }, self.direction)
    }
}

/// The Supertrend whose turns the trend gates, a band the last bar lacked read as this bar's (`supertrend_strategy_rcrbto`,
/// `pps`'s second trend, `supertrend_atr_with_trailing_stop_loss`, `supertrend_strategy`, `macandles_linearregression_strategy`),
/// and with `on_close` its bands carried on this bar's price (`pmax_explorer`, the profit maximizers, the trend trackers):
/// `price` judged and carrying, the bands `factor * atr` about `centre`. `(line, direction)`, `1` up.
struct StGated {
    factor: f64,
    on_close: bool,
    up: f64,
    down: f64,
    last: f64,
    trend: f64,
}

impl StGated {
    fn new(factor: f64, on_close: bool) -> Self {
        let nan = f64::NAN;
        StGated { factor, on_close, up: nan, down: nan, last: nan, trend: 1.0 }
    }

    fn next(&mut self, price: f64, centre: f64, atr: f64) -> (f64, f64) {
        let or = |previous: f64, now: f64| if previous.is_nan() { now } else { previous };
        let carry = if self.on_close { price } else { self.last };
        let up = centre - self.factor * atr;
        let up_before = or(self.up, up);
        let up = if carry > up_before { up.max(up_before) } else { up };
        let down = centre + self.factor * atr;
        let down_before = or(self.down, down);
        let down = if carry < down_before { down.min(down_before) } else { down };
        self.trend = if self.trend == -1.0 && price > down_before {
            1.0
        } else if self.trend == 1.0 && price < up_before {
            -1.0
        } else {
            self.trend
        };
        (self.up, self.down, self.last) = (up, down, price);
        (if self.trend == 1.0 { up } else { down }, self.trend)
    }
}

/// The Supertrend judged against the last bar's bands about a line of the rule's (`pivot_point_supertrend_backtest`, `pps`'s
/// pivot trend, `golden_triangle_strategy`, `super_trend_v_strategy_version`, and `easy_sys_420` on the open): `price` judged
/// and carrying, the bands `factor * atr` about `centre`, up at first. `(line, direction)`, `1` up.
struct StCentred {
    factor: f64,
    up: f64,
    down: f64,
    last: f64,
    trend: f64,
}

impl StCentred {
    fn new(factor: f64) -> Self {
        StCentred { factor, up: f64::NAN, down: f64::NAN, last: f64::NAN, trend: 1.0 }
    }

    fn next(&mut self, price: f64, centre: f64, atr: f64) -> (f64, f64) {
        let up = centre - self.factor * atr;
        let down = centre + self.factor * atr;
        let up = if self.last > self.up { up.max(self.up) } else { up };
        let down = if self.last < self.down { down.min(self.down) } else { down };
        self.trend = if price > self.down {
            1.0
        } else if price < self.up {
            -1.0
        } else {
            self.trend
        };
        (self.up, self.down, self.last) = (up, down, price);
        (if self.trend == 1.0 { up } else { down }, self.trend)
    }
}

/// `next_centred` over the given numbers, `[line, direction]` a bar.
fn st_centred(
    mut indicator: Supertrend,
    price: &[f64],
    centre: &[f64],
    atr: &[f64],
) -> [Vec<f64>; 2] {
    let inputs = price.iter().zip(centre).zip(atr);
    let rows: Vec<[f64; 2]> =
        inputs.map(|((&p, &c), &a)| indicator.next_centred(p, c, a)).collect();
    [col(&rows, 0), col(&rows, 1)]
}

/// A lab law over the given numbers, `[line, direction]` a bar.
fn st_lab(
    mut lab: impl FnMut(f64, f64, f64) -> (f64, f64),
    price: &[f64],
    centre: &[f64],
    atr: &[f64],
) -> [Vec<f64>; 2] {
    let inputs = price.iter().zip(centre).zip(atr);
    let (line, dir) = inputs.map(|((&p, &c), &a)| lab(p, c, a)).unzip();
    [line, dir]
}

/// Set under `down_start`, `held_direction` IS `strategy_hma_50_supertrend_sniper`'s Supertrend bit for bit on every set (the
/// direction, its sign turned, from the first bar, the line from `lookback`), and on prices (the last close and the lower band
/// over 0 at the first bar with an ATR, where the rule's own bands are the ones carried from 0) `elliott_wave_with_supertrend_exit_strategy_prese`'s;
/// the stream equals `vectorize` and survives `reset`. Without it the trend after bands that met reads down, which the grid
/// reaches; alone, or under `previous_bands`, it reads nothing.
#[test]
fn st_supertrend_held_direction_is_the_labs_flag_trend() {
    let mut differs = false;
    for (name, b) in &st_sets() {
        for (n, factor) in [(10.0, 3.0), (14.0, 2.0), (1.0, 0.5), (10.0, 15.0)] {
            let tag = format!("{name} supertrend({n}, {factor}) down start, held direction");
            let set = Supertrend::new(n, factor).unwrap().with_down_start(true);
            let plain = checked_stream(&tag, Box::new(set.clone()), b);
            let set = set.with_held_direction(true);
            let lookback = set.lookback();
            let got = checked_stream(&tag, Box::new(set), b);
            differs |= !same(&plain[1], &got[1]);
            let mut lab = StFlagTrend::new(n, factor);
            let (line, dir): (Vec<f64>, Vec<f64>) = b.iter().map(|bar| lab.next(bar)).unzip();
            let turned: Vec<f64> = dir.iter().map(|d| -d).collect();
            assert!(same(&got[1], &turned), "{tag}: direction");
            assert!(same(&got[0][lookback..], &line[lookback..]), "{tag}: line");
            if n > 1.0 && ["hourly", "walk at 1e6", "walk at 100"].contains(&name.as_str()) {
                let mut late = StLateTrend::new(n, factor);
                let (line, dir): (Vec<f64>, Vec<f64>) = b.iter().map(|bar| late.next(bar)).unzip();
                let turned: Vec<f64> = dir.iter().map(|d| -d).collect();
                assert!(same(&got[1], &turned), "{tag}: the late trend's direction");
                assert!(same(&got[0][lookback..], &line[lookback..]), "{tag}: late line");
            }
        }
    }
    assert!(differs, "held direction changed nothing");
    let b = hourly_bars();
    let row = registry("supertrend", &[10.0, 3.0], &b);
    let alone = Supertrend::new(10.0, 3.0).unwrap().with_held_direction(true);
    assert_lines_same("held alone", &checked_stream("held alone", Box::new(alone), &b), &row);
    let previous = Supertrend::new(10.0, 3.0).unwrap().with_previous_bands(true);
    let want = checked_stream("previous", Box::new(previous.clone()), &b);
    let held = previous.with_held_direction(true);
    assert_lines_same("held, previous", &checked_stream("held", Box::new(held), &b), &want);
}

/// Set under `previous_bands`, `gated_turns` IS the gated Supertrend bit for bit, both lines from the first bar: over bars
/// (the `atr` row's ATR about the midpoint, `supertrend_strategy_rcrbto`), and through `next_centred` about a rule's line with
/// a fresh-sum SMA of the true range (`supertrend_strategy`); the stream equals `vectorize` and survives `reset`. Without it
/// the trend turns either way past either band, which the grid reaches; alone, or under `down_start`, it reads nothing.
#[test]
fn st_supertrend_gated_turns_is_the_labs_gated_trend() {
    let mut differs = false;
    for (name, b) in &st_sets() {
        let close: Vec<f64> = b.iter().map(|bar| bar.close).collect();
        let mid: Vec<f64> = b.iter().map(|bar| (bar.high + bar.low) / 2.0).collect();
        let centre = st_centre(b);
        for (n, factor) in [(10.0, 3.0), (1.0, 3.0), (7.0, 0.5), (10.0, 8.5)] {
            let tag = format!("{name} supertrend({n}, {factor}) previous bands, gated turns");
            let set = Supertrend::new(n, factor).unwrap().with_previous_bands(true);
            let plain = checked_stream(&tag, Box::new(set.clone()), b);
            let set = set.with_gated_turns(true);
            let got = checked_stream(&tag, Box::new(set.clone()), b);
            differs |= !same(&plain[1], &got[1]);
            let mut lab = StGated::new(factor, false);
            let want = st_lab(|p, c, a| lab.next(p, c, a), &close, &mid, &st_atr(b, n));
            assert_lines_same(&tag, &got, &want);
            let sma = st_sma_atr(b, n);
            let mut lab = StGated::new(factor, false);
            let want = st_lab(|p, c, a| lab.next(p, c, a), &close, &centre, &sma);
            let got = st_centred(set, &close, &centre, &sma);
            assert_lines_same(&format!("{tag}, centred"), &got, &want);
        }
    }
    assert!(differs, "gated turns changed nothing");
    let b = hourly_bars();
    let row = registry("supertrend", &[10.0, 3.0], &b);
    let alone = Supertrend::new(10.0, 3.0).unwrap().with_gated_turns(true);
    assert_lines_same("gated alone", &checked_stream("gated alone", Box::new(alone), &b), &row);
    let down = Supertrend::new(10.0, 3.0).unwrap().with_down_start(true);
    let want = checked_stream("down", Box::new(down.clone()), &b);
    let gated = down.with_gated_turns(true);
    assert_lines_same("gated, down", &checked_stream("gated", Box::new(gated), &b), &want);
}

/// Set with `previous_bands` and `gated_turns`, `carry_on_close` IS the profit maximizer's line bit for bit through
/// `next_centred` with an average as both the price and the centre (`pmax_explorer`, `profit_maximizer_strategy`,
/// `profit_maximizer_strategy_long_short`, `pmax_on_rsi_w_t3_strategy`: the `atr` row's ATR) and the trend tracker's with a
/// percent of a zero-started average as the band (`optimized_trend_tracker` and its stochastic and twin kin), and over bars the
/// gated law carried on this close; the stream equals `vectorize` and survives `reset`. Unset, the last close carries; alone it
/// reads nothing.
#[test]
fn st_supertrend_carry_on_close_is_the_profit_maximizers_line() {
    let mut differs = false;
    for (name, b) in &st_sets() {
        let close: Vec<f64> = b.iter().map(|bar| bar.close).collect();
        let mid: Vec<f64> = b.iter().map(|bar| (bar.high + bar.low) / 2.0).collect();
        let mut ema = vike_indicators::indicators::Ema::new(10.0).unwrap();
        let average: Vec<f64> = mid.iter().map(|&m| ema.next_value(m)).collect();
        let mut zero = vike_indicators::indicators::Ema::new(2.0).unwrap().with_zero_seed(true);
        let tracked: Vec<f64> = close.iter().map(|&c| zero.next_value(c)).collect();
        let percent: Vec<f64> = tracked.iter().map(|&m| m * 1.4 * 0.01).collect();
        for (n, factor) in [(10.0, 3.0), (1.0, 0.5), (14.0, 2.0)] {
            let tag = format!("{name} supertrend({n}, {factor}) gated, carry on close");
            let gated = Supertrend::new(n, factor).unwrap().with_previous_bands(true);
            let gated = gated.with_gated_turns(true);
            let plain = checked_stream(&tag, Box::new(gated.clone()), b);
            let set = gated.with_carry_on_close(true);
            let got = checked_stream(&tag, Box::new(set.clone()), b);
            differs |= !same(&plain[0], &got[0]);
            let atr = st_atr(b, n);
            let mut lab = StGated::new(factor, true);
            let want = st_lab(|p, c, a| lab.next(p, c, a), &close, &mid, &atr);
            assert_lines_same(&tag, &got, &want);
            let mut lab = StGated::new(factor, true);
            let want = st_lab(|p, c, a| lab.next(p, c, a), &average, &average, &atr);
            let pmax = st_centred(set.clone(), &average, &average, &atr);
            assert_lines_same(&format!("{tag}: the profit maximizer"), &pmax, &want);
            let tracker = Supertrend::new(n, 1.0)
                .unwrap()
                .with_carry_on_close(true)
                .with_gated_turns(true)
                .with_previous_bands(true);
            let mut lab = StGated::new(1.0, true);
            let want = st_lab(|p, c, a| lab.next(p, c, a), &tracked, &tracked, &percent);
            let got = st_centred(tracker, &tracked, &tracked, &percent);
            assert_lines_same(&format!("{tag}: the trend tracker"), &got, &want);
        }
    }
    assert!(differs, "carry on close changed nothing");
    let b = hourly_bars();
    let row = registry("supertrend", &[10.0, 3.0], &b);
    let alone = Supertrend::new(10.0, 3.0).unwrap().with_carry_on_close(true);
    assert_lines_same("carry alone", &checked_stream("carry alone", Box::new(alone), &b), &row);
}

/// `next_centred` fed the bar's own close, midpoint and ATR IS `next`, bit for bit, in every form (the registry row's over
/// Wilder's average of the true ranges from the second bar, the others over the `atr` row's ATR); about a rule's line, on the
/// close and on the open, it IS the lab's centred Supertrend (`pivot_point_supertrend_backtest`, `easy_sys_420`).
#[test]
fn st_supertrend_next_centred_is_the_labs_centred_trend() {
    for (name, b) in &st_sets() {
        let close: Vec<f64> = b.iter().map(|bar| bar.close).collect();
        let open: Vec<f64> = b.iter().map(|bar| bar.open).collect();
        let mid: Vec<f64> = b.iter().map(|bar| (bar.high + bar.low) * 0.5).collect();
        let centre = st_centre(b);
        for (n, factor) in [(10.0, 3.0), (1.0, 1.0), (50.0, 3.0)] {
            let base = || Supertrend::new(n, factor).unwrap();
            let (atr, skip) = (st_atr(b, n), st_skip_atr(b, n));
            let previous = base().with_previous_bands(true);
            let gated = previous.clone().with_gated_turns(true);
            let carried = gated.clone().with_carry_on_close(true);
            let down = base().with_down_start(true);
            let held = down.clone().with_held_direction(true);
            let forms = [
                ("registry", base(), &skip),
                ("previous bands", previous, &atr),
                ("gated", gated, &atr),
                ("carry on close", carried, &atr),
                ("down start", down, &atr),
                ("held", held, &atr),
            ];
            for (form, indicator, atr) in forms {
                let tag = format!("{name} supertrend({n}, {factor}) {form}");
                let mut by_bar = indicator.clone();
                let rows: Vec<[f64; 2]> = b.iter().map(|bar| by_bar.next(bar)).collect();
                let want = [col(&rows, 0), col(&rows, 1)];
                assert_lines_same(&tag, &st_centred(indicator, &close, &mid, atr), &want);
            }
            for (price, on) in [(&close, "close"), (&open, "open")] {
                let tag = format!("{name} supertrend({n}, {factor}) about a line, on the {on}");
                let mut lab = StCentred::new(factor);
                let want = st_lab(|p, c, a| lab.next(p, c, a), price, &centre, &atr);
                let got = st_centred(base().with_previous_bands(true), price, &centre, &atr);
                assert_lines_same(&tag, &got, &want);
            }
        }
    }
}

// ---------------------------------------------------------------- I6: wavetrend, qqe, stc, percent_rank
//
// The four oscillator rows the strategy lab used to compute by hand, each against its published
// definition written out as a slice loop (`crates/vike-indicators/src/indicators/streams_osc.rs`
// names the sources). The averages are the crate's EMA as its doc states it: the mean of the first
// `n` values (`sum * (1 / n)`), then `alpha * x + (1 - alpha) * previous`, `alpha = 2 / (n + 1)`.

use vike_indicators::indicators::{Ema, PercentRank, Qqe, Stc, Wavetrend};

/// The crate's EMA over the numbers of `x` only: a `NaN` position stays `NaN` and moves nothing.
fn osc_ema(x: &[f64], n: usize) -> Vec<f64> {
    let (alpha, mut seen, mut sum, mut previous) = (2.0 / (n as f64 + 1.0), 0usize, 0.0, f64::NAN);
    x.iter()
        .map(|&v| {
            if v.is_nan() {
                return f64::NAN;
            }
            seen += 1;
            if seen < n {
                sum += v;
                f64::NAN
            } else if seen == n {
                sum += v;
                previous = sum * (1.0 / n as f64);
                previous
            } else {
                previous = alpha * v + (1.0 - alpha) * previous;
                previous
            }
        })
        .collect()
}

/// The mean of the last `n` numbers of `x`, from the first `n` numbers on; `NaN` where `x` is.
fn osc_sma(x: &[f64], n: usize) -> Vec<f64> {
    let mut window: Vec<f64> = Vec::new();
    x.iter()
        .map(|&v| {
            if v.is_nan() {
                return f64::NAN;
            }
            window.push(v);
            if window.len() > n {
                window.remove(0);
            }
            if window.len() == n { window.iter().sum::<f64>() / n as f64 } else { f64::NAN }
        })
        .collect()
}

/// WaveTrend's definition: `wt1 = EMA(ci, average)`, `ci = (ap - esa) / (0.015 * d)`,
/// `esa = EMA(ap, channel)`, `d = EMA(|ap - esa|, channel)`, `wt2 = SMA(wt1, 4)`.
fn wavetrend_ref(ap: &[f64], channel: usize, average: usize) -> [Vec<f64>; 2] {
    let esa = osc_ema(ap, channel);
    let dev: Vec<f64> = ap.iter().zip(&esa).map(|(a, e)| (a - e).abs()).collect();
    let d = osc_ema(&dev, channel);
    let ci: Vec<f64> = (0..ap.len()).map(|i| (ap[i] - esa[i]) / (0.015 * d[i])).collect();
    let wt1 = osc_ema(&ci, average);
    let wt2 = osc_sma(&wt1, 4);
    [wt1, wt2]
}

fn typical(b: &[Bar]) -> Vec<f64> {
    b.iter().map(|bar| (bar.high + bar.low + bar.close) / 3.0).collect()
}

/// A worked example by hand, `channel = average = 2` over `ap = 1, 2, 3, 5, 4`: `esa` is 3/2, 5/2,
/// 25/6, 73/18 from the second value, `d` 1/2, 13/18, 5/18 from the third, so `ci` is
/// `1 / 0.015`, `(15 / 13) / 0.015` and `-1 / 0.075`, and `wt1` their EMA(2): `(14 / 13) / 0.015`,
/// then `(44 / 195) / 0.015`.
#[test]
fn wavetrend_worked_example() {
    let mut w = Wavetrend::new(2.0, 2.0).unwrap();
    let got: Vec<f64> = [1.0, 2.0, 3.0, 5.0, 4.0].iter().map(|&x| w.next_value(x)[0]).collect();
    assert!(got[..3].iter().all(|v| v.is_nan()), "{got:?}");
    assert!((got[3] - 14.0 / 13.0 / 0.015).abs() < 1e-11, "{got:?}");
    assert!((got[4] - 44.0 / 195.0 / 0.015).abs() < 1e-11, "{got:?}");
    assert_eq!(w.lookback(), 3);
}

/// The stream is the definition over the typical price, the signal line to the rounding of a
/// running sum; the registry row, the typed door and `series::wavetrend` agree bit for bit.
#[test]
fn wavetrend_is_the_definition() {
    for (name, b) in &wave_two_sets() {
        for (channel, average) in [(10.0, 21.0), (6.0, 23.0), (14.0, 10.0), (2.0, 2.0)] {
            let tag = format!("{name} wavetrend({channel}, {average})");
            let got = checked_stream(&tag, Box::new(Wavetrend::new(channel, average).unwrap()), b);
            let want = wavetrend_ref(&typical(b), channel as usize, average as usize);
            assert!(same(&got[0], &want[0]), "{tag}: wavetrend line");
            assert_close(&tag, &got[1], &want[1], 1e-9, 1.0);
            assert_lines_same(&tag, &registry("wavetrend", &[channel, average], b), &got);
            let h: Vec<f64> = b.iter().map(|x| x.high).collect();
            let l: Vec<f64> = b.iter().map(|x| x.low).collect();
            let s = series::wavetrend(&h, &l, &closes_of(b), channel, average).unwrap();
            assert_lines_same(&tag, &s, &got);
        }
    }
}

fn closes_of(b: &[Bar]) -> Vec<f64> {
    b.iter().map(|x| x.close).collect()
}

/// QQE's definition over an RSI line: `fast = EMA(rsi, smoothing)`, the range
/// `EMA(EMA(|fast - fast[1]|, w), w) * factor`, `w = 2 * rsi_period - 1`, the long trail rising only
/// while `fast` was and is above it, the short trail falling only while `fast` was and is under it,
/// the state up at the start, up on a cross of the short trail, down on a cross of the long one.
fn qqe_ref(rsi: &[f64], rsi_period: usize, smoothing: usize, factor: f64) -> [Vec<f64>; 2] {
    let w = 2 * rsi_period - 1;
    let fast = osc_ema(rsi, smoothing);
    let moves: Vec<f64> = (0..fast.len())
        .map(|i| if i == 0 { f64::NAN } else { (fast[i - 1] - fast[i]).abs() })
        .collect();
    let range = osc_ema(&osc_ema(&moves, w), w);
    let nan_or = |a: f64, b: f64, f: fn(f64, f64) -> f64| {
        if a.is_nan() || b.is_nan() { f64::NAN } else { f(a, b) }
    };
    let (mut long, mut short) = (vec![f64::NAN; fast.len()], vec![f64::NAN; fast.len()]);
    let mut trail = vec![f64::NAN; fast.len()];
    let mut up = true;
    let at = |v: &[f64], i: usize, back: usize| if i >= back { v[i - back] } else { f64::NAN };
    for i in 0..fast.len() {
        let (f, f1) = (fast[i], at(&fast, i, 1));
        let (r, l1, s1) = (range[i] * factor, at(&long, i, 1), at(&short, i, 1));
        long[i] = if f1 > l1 && f > l1 { nan_or(l1, f - r, f64::max) } else { f - r };
        short[i] = if f1 < s1 && f < s1 { nan_or(s1, f + r, f64::min) } else { f + r };
        let side = |a: f64, b: f64| i8::from(a > b) - i8::from(a < b);
        let flipped = |b1: f64, b2: f64| {
            let (now, then) = (side(f, b1), side(f1, b2));
            !(f.is_nan() || b1.is_nan() || f1.is_nan() || b2.is_nan()) && now != 0 && now != then
        };
        if flipped(s1, at(&short, i, 2)) {
            up = true;
        } else if flipped(l1, at(&long, i, 2)) {
            up = false;
        }
        trail[i] = if up { long[i] } else { short[i] };
    }
    [fast, trail]
}

#[test]
fn qqe_is_the_definition() {
    for (name, b) in &wave_two_sets() {
        let c = closes_of(b);
        for (n, sf, factor) in [(14.0, 5.0, 4.236), (6.0, 6.0, 3.0), (2.0, 1.0, 0.1)] {
            for rs_form in [false, true] {
                let tag = format!("{name} qqe({n}, {sf}, {factor}) rs_form {rs_form}");
                let typed = || Qqe::new(n, sf, factor).unwrap().with_rs_form(rs_form);
                let got = checked_stream(&tag, Box::new(typed()), b);
                let mut rsi = Rsi::new(n).unwrap().with_rs_form(rs_form);
                let rsi: Vec<f64> = c.iter().map(|&x| rsi.next_value(x)).collect();
                let want = qqe_ref(&rsi, n as usize, sf as usize, factor);
                assert_lines_same(&tag, &got, &want);
                let unset = registry("qqe", &[n, sf, factor], b);
                if !rs_form {
                    assert_lines_same(&tag, &unset, &got);
                    let s = series::qqe(&c, n, sf, factor).unwrap();
                    assert_lines_same(&tag, &s, &got);
                }
            }
        }
        let trails = registry("qqe", &[14.0, 5.0, 4.236], b);
        assert!(trails[1].iter().any(|v| !v.is_nan()), "{name}: the trail is reached");
    }
    let q = Qqe::default();
    assert_eq!((q.lookback(), q.lookback_full()), (14 + 4, 18 + 1 + 2 * 26));
}

/// The Schaff trend cycle as the published formula steps it over the MACD (the crate's EMAs): `%K`
/// of the MACD in its `cycle`-value range, kept while the range is not positive (0 before), `%D`
/// halved toward `%K` from the first bar, and the same over `%D`. A window is `NaN` (no range)
/// until `cycle` values have come; its extremes are those of its numbers.
fn stc_ref(c: &[f64], fast: f64, slow: f64, cycle: usize) -> Vec<f64> {
    let (mut f, mut s) = (Ema::new(fast).unwrap(), Ema::new(slow).unwrap());
    let macd: Vec<f64> = c.iter().map(|&x| f.next_value(x) - s.next_value(x)).collect();
    let extremes = |v: &[f64], i: usize| -> (f64, f64) {
        if i + 1 < cycle {
            return (f64::NAN, f64::NAN);
        }
        let w: Vec<f64> = v[i + 1 - cycle..=i].iter().copied().filter(|x| !x.is_nan()).collect();
        if w.is_empty() {
            return (f64::NAN, f64::NAN);
        }
        (
            w.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            w.iter().copied().fold(f64::INFINITY, f64::min),
        )
    };
    let (mut k, mut kd) = (0.0, 0.0);
    let (mut d, mut out) = (vec![f64::NAN; c.len()], vec![f64::NAN; c.len()]);
    for i in 0..c.len() {
        let (hi, lo) = extremes(&macd, i);
        if hi - lo > 0.0 {
            k = (macd[i] - lo) / (hi - lo) * 100.0;
        }
        d[i] = if i == 0 { k } else { d[i - 1] + 0.5 * (k - d[i - 1]) };
        let (hi, lo) = extremes(&d, i);
        if hi - lo > 0.0 {
            kd = (d[i] - lo) / (hi - lo) * 100.0;
        }
        out[i] = if i == 0 { kd } else { out[i - 1] + 0.5 * (kd - out[i - 1]) };
    }
    out
}

#[test]
fn stc_is_the_published_formula() {
    for (name, b) in &wave_two_sets() {
        let c = closes_of(b);
        for (fast, slow, cycle) in [(23.0, 50.0, 10.0), (27.0, 50.0, 80.0), (2.0, 3.0, 2.0)] {
            let tag = format!("{name} stc({fast}, {slow}, {cycle})");
            let got = checked_stream(&tag, Box::new(Stc::new(fast, slow, cycle).unwrap()), b);
            assert!(same(&got[0], &stc_ref(&c, fast, slow, cycle as usize)), "{tag}");
            assert!(got[0].iter().all(|v| (0.0..=100.0).contains(v)), "{tag}: inside 0..=100");
            assert_lines_same(&tag, &registry("stc", &[fast, slow, cycle], b), &got);
            assert!(same(&series::stc(&c, fast, slow, cycle).unwrap(), &got[0]), "{tag}: series");
        }
    }
}

/// Percent rank by hand, then against the count over every window.
#[test]
fn percent_rank_is_the_empirical_distribution_of_the_window() {
    let rank = |x: &[f64], n: f64| series::percent_rank(x, n).unwrap();
    let got = rank(&[3.0, 1.0, 4.0, 1.0, 5.0, 9.0, 2.0, 6.0], 4.0);
    assert!(got[..4].iter().all(|v| v.is_nan()), "{got:?}");
    assert_eq!(got[4..], [100.0, 100.0, 25.0, 75.0]);
    assert_eq!(rank(&[2.0; 5], 4.0)[4], 100.0, "a tie is at or below");
    assert_eq!(rank(&[5.0, 4.0, 3.0, 2.0, 1.0], 4.0)[4], 0.0);
    assert!(rank(&[1.0, 2.0, f64::NAN], 2.0)[2].is_nan(), "a NaN value has no rank");
    assert_eq!(rank(&[f64::NAN, 1.0, 2.0], 2.0)[2], 50.0, "a NaN in the window is not at or below");
    for (name, b) in &wave_two_sets() {
        let c = closes_of(b);
        for n in [1usize, 19, 150] {
            let tag = format!("{name} percent_rank({n})");
            let got = checked_stream(&tag, Box::new(PercentRank::new(n as f64).unwrap()), b);
            let want: Vec<f64> = (0..c.len())
                .map(|i| {
                    if i < n || c[i].is_nan() {
                        return f64::NAN;
                    }
                    let under = c[i - n..i].iter().filter(|&&v| v <= c[i]).count();
                    100.0 * under as f64 / n as f64
                })
                .collect();
            assert!(same(&got[0], &want), "{tag}");
            assert_lines_same(&tag, &registry("percent_rank", &[n as f64], b), &got);
            assert_eq!(PercentRank::new(n as f64).unwrap().lookback(), n);
        }
    }
}
