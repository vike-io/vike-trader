// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Accurate BUY & SELL 5 mins TF by RR" by RohitVR, https://www.tradingview.com/script/jVZvCaUh/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2022-03-22, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! A score of sixteen readings, each worth its weight on the bar it holds, summed for each side; a side whose score reaches 10
//! is a strong signal. The readings, long side (the short side mirrors each):
//! - the close over EMA(200) (2), and EMA(21) rising while EMA(55) falls (2);
//! - the Supertrend (10 bars, factor 3) turning up (4), and the parabolic SAR (0.02 up by 0.01 to 0.2) turning up, under the
//!   close after being over the last close (3; the short side compares the last SAR with this bar's close);
//! - HMA(100) under the close while the last HMA exists (1), and the HMA turning up, a trough on the last bar (3);
//! - a regular bullish divergence of RSI(14) on this bar or one of the two before (4), and RSI over 50 after being under it one
//!   or two bars back (1);
//! - the MACD(12, 26, 9) histogram turning up below 0 (1), and the MACD crossing over its signal while the signal is below 0
//!   (1);
//! - the wave trend (9, 12) crossing over its 4-bar average while that average is under -10 (4);
//! - a regular bullish divergence of the stochastic's %K (14, 3) on this bar or one of the two before (1), and %K crossing
//!   over %D (3) (1);
//! - the turn up of a stop line 1.85 true ranges from the bar's high and low (1);
//! - the relative volatility index (a 12-bar standard deviation, EMAs of 14) over 25 after being under 40 (3), and its
//!   regular bullish divergence on this bar or one of the two before (4).
//!
//! A divergence is the original's: pivots 6 bars left and 2 right, the pivot's value under the last pivot's while the price
//! two bars back (the low; the high for a bearish one) is under its own then, 5 to 60 bars from the bar before to the last
//! pivot. The signals alternate: a strong long counts only after a strong short (or none), and the other way, and a bar strong
//! on both sides ends short. Long 15% of the account's worth at the next open on the first long signal, closing a short; short
//! on the mirror. A long also closes once the close crosses under 97.5% of its average price, a short once it crosses over
//! 102.5% of it (the original's stop-loss warning). No costs; starts with 1,000.
//!
//! The original's description credits ZacVaughn for its first version. Readings weighted 0 by default (the SuperTrend and SAR
//! sides, RSI levels, MACD, wave trend and OBV divergences, Bollinger contact, support and resistance crosses, the money flow,
//! VWAP and engulfing candles) decide nothing and are left out, as are its weak signals, table, labels and alerts. It does
//! not look into the future: its only lookahead reads (earnings, dividends and splits) anchor a VWAP only under anchors it
//! never takes, and that VWAP weighs 0; every pivot is read two bars after its extreme.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{
    ConfirmedPivot, Ema, Hma, Macd, Psar, Rsi, Stddev, Stochastic, Supertrend, Wavetrend,
};
use vike_indicators::strategy_kit::parabolic_sar;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const STRONG: i32 = 10;
const SHARE: f64 = 15.0;
const STOP_PERCENT: f64 = -2.5;
const STOP_RANGES: f64 = 1.85;
const LONG: &str = "longposition";
const SHORT: &str = "shortposition";

/// One side's pivots: `(bar, oscillator, price)` of the newest and the one before it, both read two bars back at their bar.
#[derive(Default)]
struct Side {
    last: Option<(usize, f64, f64)>,
    before: Option<(usize, f64, f64)>,
}

impl Side {
    /// Bars from the bar before `bar` back to the newest pivot before `bar`, 5 to 60.
    fn in_range(&self, bar: usize) -> bool {
        self.last.is_some_and(|(at, _, _)| (5..=60).contains(&(bar - 1 - at)))
    }

    fn found(&mut self, at: (usize, f64, f64)) {
        self.before = self.last.replace(at);
    }
}

/// The regular divergences of one oscillator against the price, and whether one showed on this bar or the two before.
struct Divergence {
    pivots: ConfirmedPivot,
    /// `(oscillator, low, high)` of this bar and the two before, newest first.
    window: [(f64, f64, f64); 3],
    lows: Side,
    highs: Side,
    bull: [bool; 3],
    bear: [bool; 3],
}

impl Divergence {
    fn new() -> Self {
        Divergence {
            pivots: ConfirmedPivot::new(6.0, 2.0).expect("valid spans"),
            window: [(f64::NAN, f64::NAN, f64::NAN); 3],
            lows: Side::default(),
            highs: Side::default(),
            bull: [false; 3],
            bear: [false; 3],
        }
    }

    /// Feeds bar `n` and answers `(bullish, bearish)` on it or one of the two bars before.
    fn next(&mut self, n: usize, oscillator: f64, low: f64, high: f64) -> (bool, bool) {
        self.window = [(oscillator, low, high), self.window[0], self.window[1]];
        let [pivot_high, pivot_low, ..] = self.pivots.next_hl(oscillator, oscillator);
        let (back, low_back, high_back) = self.window[2];
        let mut bull = false;
        if !pivot_low.is_nan() {
            let in_range = self.lows.in_range(n);
            self.lows.found((n, back, low_back));
            bull = self.lows.before.is_some_and(|(_, o, p)| low_back < p && back > o && in_range);
        }
        let mut bear = false;
        if !pivot_high.is_nan() {
            let in_range = self.highs.in_range(n);
            self.highs.found((n, back, high_back));
            bear = self.highs.before.is_some_and(|(_, o, p)| high_back > p && back < o && in_range);
        }
        self.bull = [bull, self.bull[0], self.bull[1]];
        self.bear = [bear, self.bear[0], self.bear[1]];
        (self.bull.contains(&true), self.bear.contains(&true))
    }
}

/// `weight` when `on`, else 0.
fn score(on: bool, weight: i32) -> i32 {
    if on { weight } else { 0 }
}

struct S {
    bar: usize,
    close: f64,
    ema21: Ema,
    ema55: Ema,
    ema200: Ema,
    /// The last bar's EMA(21) and EMA(55).
    fast: f64,
    slow: f64,
    trend: Supertrend,
    direction: f64,
    sar: Psar,
    last_sar: f64,
    hma: Hma,
    /// HMA one and two bars back.
    hma_back: [f64; 2],
    rsi: Rsi,
    /// RSI one and two bars back.
    rsi_back: [f64; 2],
    rsi_divergence: Divergence,
    macd: Macd,
    /// The last bar's `[macd, signal, hist]`, and the histogram two bars back.
    last_macd: [f64; 3],
    hist_two: f64,
    wave: Wavetrend,
    last_wave: [f64; 2],
    stoch: Stochastic,
    last_stoch: [f64; 2],
    stoch_divergence: Divergence,
    long_stop: f64,
    short_stop: f64,
    stop_direction: i32,
    deviation: Stddev,
    upper: Ema,
    lower: Ema,
    last_rvi: f64,
    rvi_divergence: Divergence,
    /// The signal side, 1 long, -1 short, 0 before the first.
    side: i32,
    /// The last bar's average price, NaN when flat.
    last_average: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let ema = |n: f64| Ema::new(n).expect("a valid length");
    Box::new(S {
        bar: 0,
        close: f64::NAN,
        ema21: ema(21.0),
        ema55: ema(55.0),
        ema200: ema(200.0),
        fast: f64::NAN,
        slow: f64::NAN,
        trend: Supertrend::new(10.0, 3.0).expect("valid parameters").with_down_start(true),
        direction: f64::NAN,
        sar: parabolic_sar(0.02, 0.01, 0.2),
        last_sar: f64::NAN,
        hma: Hma::new(100.0).expect("a valid length"),
        hma_back: [f64::NAN; 2],
        rsi: Rsi::new(14.0).expect("a valid length").with_rs_form(true).with_zero_saturation(true),
        rsi_back: [f64::NAN; 2],
        rsi_divergence: Divergence::new(),
        macd: Macd::new(12.0, 26.0, 9.0).expect("valid lengths"),
        last_macd: [f64::NAN; 3],
        hist_two: f64::NAN,
        wave: Wavetrend::new(9.0, 12.0).expect("valid lengths"),
        last_wave: [f64::NAN; 2],
        stoch: Stochastic::new(14.0, 3.0, 3.0).expect("valid lengths"),
        last_stoch: [f64::NAN; 2],
        stoch_divergence: Divergence::new(),
        long_stop: f64::NAN,
        short_stop: f64::NAN,
        stop_direction: 1,
        deviation: Stddev::new(12.0).expect("a valid length"),
        upper: ema(14.0).with_skip_nan(true),
        lower: ema(14.0).with_skip_nan(true),
        last_rvi: f64::NAN,
        rvi_divergence: Divergence::new(),
        side: 0,
        last_average: f64::NAN,
    })
}

impl S {
    /// The stop line's turn this bar: 1 up, -1 down, 0 none.
    fn stop_turn(&mut self, bar: &Bar) -> i32 {
        let last_close = self.close;
        let range = if last_close.is_nan() {
            bar.high - bar.low
        } else {
            (bar.high - bar.low)
                .max((bar.high - last_close).abs())
                .max((bar.low - last_close).abs())
        };
        let atr = STOP_RANGES * range;
        let long_last = if self.long_stop.is_nan() { bar.high - atr } else { self.long_stop };
        let short_last = if self.short_stop.is_nan() { bar.low + atr } else { self.short_stop };
        self.long_stop = bar.high - atr;
        if last_close > long_last {
            self.long_stop = self.long_stop.max(long_last);
        }
        self.short_stop = bar.low + atr;
        if last_close < short_last {
            self.short_stop = self.short_stop.min(short_last);
        }
        let was = self.stop_direction;
        if bar.close > short_last {
            self.stop_direction = 1;
        } else if bar.close < long_last {
            self.stop_direction = -1;
        }
        // The first bar has no direction before it.
        if self.bar == 0 || was == self.stop_direction { 0 } else { self.stop_direction }
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let n = self.bar;
        let close = bar.close;
        let last_close = self.close;

        let (fast, slow, long_trend) = (
            self.ema21.next_value(close),
            self.ema55.next_value(close),
            self.ema200.next_value(close),
        );
        let [_, direction] = self.trend.next(bar);
        let sar = self.sar.next(bar);
        let hma = self.hma.next_value(close);
        let rsi = self.rsi.next_value(close);
        let (rsi_bull, rsi_bear) = self.rsi_divergence.next(n, rsi, bar.low, bar.high);
        let [macd, signal, hist] = self.macd.next(bar);
        let [wave, wave_average] = self.wave.next(bar);
        let [k, d] = self.stoch.next(bar);
        let (stoch_bull, stoch_bear) = self.stoch_divergence.next(n, k, bar.low, bar.high);
        let turn = self.stop_turn(bar);
        let deviation = self.deviation.next_value(close);
        let change = close - last_close;
        let upper = self.upper.next_value(if change <= 0.0 { 0.0 } else { deviation });
        let lower = self.lower.next_value(if change > 0.0 { 0.0 } else { deviation });
        let rvi = upper / (upper + lower) * 100.0;
        let (rvi_bull, rvi_bear) = self.rvi_divergence.next(n, rvi, bar.low, bar.high);

        let [hma_1, hma_2] = self.hma_back;
        let [rsi_1, rsi_2] = self.rsi_back;
        let [macd_1, signal_1, hist_1] = self.last_macd;
        let [wave_1, wave_average_1] = self.last_wave;
        let [k_1, d_1] = self.last_stoch;
        let long_score = score(long_trend < close, 2)
            + score(fast > self.fast && slow < self.slow, 2)
            + score(direction > 0.0 && self.direction < 0.0, 4)
            + score(sar < close && self.last_sar > last_close, 3)
            + score(hma < close && !hma_1.is_nan() && hma_1 != 0.0, 1)
            + score(hma > hma_1 && hma_1 < hma_2, 3)
            + score(rsi_bull, 4)
            + score(rsi > 50.0 && (rsi_1 < 50.0 || rsi_2 < 50.0), 1)
            + score(hist > hist_1 && hist_1 < self.hist_two && hist < 0.0, 1)
            + score(macd > signal && macd_1 < signal_1 && signal < 0.0, 1)
            + score(wave > wave_average && wave_1 < wave_average_1 && wave_average < -10.0, 4)
            + score(stoch_bull, 1)
            + score(k > d && k_1 < d_1, 1)
            + score(turn == 1, 1)
            + score(rvi > 25.0 && self.last_rvi < 40.0, 3)
            + score(rvi_bull, 4);
        let short_score = score(long_trend > close, 2)
            + score(fast < self.fast && slow > self.slow, 2)
            + score(direction < 0.0 && self.direction > 0.0, 4)
            + score(sar > close && self.last_sar < close, 3)
            + score(hma > close, 1)
            + score(hma < hma_1 && hma_1 > hma_2, 3)
            + score(rsi_bear, 4)
            + score(rsi < 50.0 && (rsi_1 > 50.0 || rsi_2 > 50.0), 1)
            + score(hist < hist_1 && hist_1 > self.hist_two && hist > 0.0, 1)
            + score(macd < signal && macd_1 > signal_1 && signal > 0.0, 1)
            + score(wave < wave_average && wave_1 > wave_average_1 && wave_average > 10.0, 4)
            + score(stoch_bear, 1)
            + score(k < d && k_1 > d_1, 1)
            + score(turn == -1, 1)
            + score(rvi < 75.0 && self.last_rvi > 60.0, 3)
            + score(rvi_bear, 4);

        let was = self.side;
        if long_score >= STRONG && self.side <= 0 {
            self.side = 1;
        }
        if short_score >= STRONG && self.side >= 0 {
            self.side = -1;
        }
        let first = n == 0;
        let long_entry = self.side == 1 && was != 1 && !first;
        let short_entry = self.side == -1 && was != -1 && !first;

        // The stop-loss warning: the close crossing the line 2.5% beyond the average price, read on this bar and the last.
        let position = strategy.position();
        let average = strategy.avg_cost();
        let fraction = STOP_PERCENT / 100.0;
        let (long_line, short_line) = (average * (1.0 + fraction), average * (1.0 - fraction));
        let (last_long_line, last_short_line) =
            (self.last_average * (1.0 + fraction), self.last_average * (1.0 - fraction));
        let stop_long = long_line > close && last_long_line < last_close && position > 0.0;
        let stop_short = short_line < close && last_short_line > last_close && position < 0.0;

        let size = share_of_worth(&strategy.sizing(), SHARE, close);
        if long_entry {
            strategy.entry(LONG, Long).size(size);
        }
        if short_entry || stop_long {
            strategy.close(LONG);
        }
        if short_entry {
            strategy.entry(SHORT, Short).size(size);
        }
        if long_entry || stop_short {
            strategy.close(SHORT);
        }

        self.bar += 1;
        self.close = close;
        (self.fast, self.slow) = (fast, slow);
        self.direction = direction;
        self.last_sar = sar;
        self.hma_back = [hma, hma_1];
        self.rsi_back = [rsi, rsi_1];
        self.hist_two = hist_1;
        self.last_macd = [macd, signal, hist];
        self.last_wave = [wave, wave_average];
        self.last_stoch = [k, d];
        self.last_rvi = rvi;
        self.last_average = average;
    }
}
