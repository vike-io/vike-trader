// SPDX-License-Identifier: Apache-2.0 AND MPL-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-ies-integration-01/strategy.pine
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived in part from "Integrated Execution System [JOAT]", © officialjackofalltrades, https://www.tradingview.com/script/SxASLpF5-Integrated-Execution-System-JOAT/
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Adds up four layers for each side: 2 points when `adx_regime_classify`'s regime is trending with that DI line on top, and 1 point
//! each for `three_ema_bias_score`'s bias, `rsi_macd_momentum`'s two votes, and pressure, which is `pressure_gauge`'s average beyond
//! 0.55 or 0.45 or its change beyond 0.05. All 5 points while trending, at least 8 bars after the last entry, enter or flip to that
//! side. Close when the regime stops trending. Only an order placed resets the 8 bars.
//!
//! The bar count and the side held remember where the run began, and a signal sees no broker to correct them.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::Atr;
use vike_indicators::indicators::Ema;
use vike_indicators::indicators::Sma;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

const TREND_POINTS: u8 = 2;
const POINTS_NEEDED: u8 = 5;
const COOLDOWN_BARS: u32 = 8;

struct S {
    regime: RegimeGauge,
    bias: BiasScore,
    momentum: MomentumVotes,
    pressure: PressureChain,
    // Starts far past the cooldown, so the first entry waits on nothing.
    bars_since_entry: u32,
    held: i8,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        regime: RegimeGauge::new(),
        bias: BiasScore::new(),
        momentum: MomentumVotes::new(),
        pressure: PressureChain::new(),
        bars_since_entry: 999,
        held: 0,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let (trending, plus_di, minus_di) = self.regime.step(bar);
        let (bias_bull, bias_bear) = self.bias.step(bar);
        let (votes_bull, votes_bear) = self.momentum.step(bar);
        let (pressure, change) = self.pressure.step(bar);

        let tally = |di_on_top: bool, bias: bool, votes: u8, pressing: bool| {
            u8::from(trending && di_on_top) * TREND_POINTS
                + u8::from(bias)
                + u8::from(votes >= VOTES_NEEDED)
                + u8::from(pressing)
        };
        let bull = tally(
            plus_di > minus_di,
            bias_bull,
            votes_bull,
            pressure > MIDDLE + BAND || change > BAND,
        );
        let bear = tally(
            minus_di > plus_di,
            bias_bear,
            votes_bear,
            pressure < MIDDLE - BAND || change < -BAND,
        );

        self.bars_since_entry += 1;
        let ready = trending && self.bars_since_entry >= COOLDOWN_BARS;
        // Full points need the trend points, which one DI line takes at a time, so the two entries cannot both fire.
        if ready && bull >= POINTS_NEEDED && self.held <= 0 {
            self.bars_since_entry = 0;
            self.held = 1;
            Signal::Long
        } else if ready && bear >= POINTS_NEEDED && self.held >= 0 {
            self.bars_since_entry = 0;
            self.held = -1;
            Signal::Short
        } else if !trending && self.held != 0 {
            self.held = 0;
            Signal::Close
        } else {
            Signal::Hold
        }
    }
}

const SMOOTHING: usize = 14;
const TREND_ADX: f64 = 25.0;
const WIDE_ATR: f64 = 1.4;
const NARROW_ATR: f64 = 0.6;

/// Wilder's average: the mean of its first `period` known values, then each value weighted `1 / period`. A NaN is skipped, so
/// a line missing its first bar seeds one bar later than its neighbours.
struct Wilder {
    period: usize,
    known: usize,
    sum: f64,
    level: f64,
}

impl Wilder {
    fn new(period: usize) -> Self {
        Wilder { period, known: 0, sum: 0.0, level: f64::NAN }
    }
    fn push(&mut self, value: f64) -> f64 {
        if value.is_nan() {
            return self.level;
        }
        self.known += 1;
        if self.known < self.period {
            self.sum += value;
        } else if self.known == self.period {
            self.level = (self.sum + value) / self.period as f64;
        } else {
            let share = 1.0 / self.period as f64;
            self.level = share * value + (1.0 - share) * self.level;
        }
        self.level
    }
}

/// The regime reading, copied from `adx_regime_classify`.
struct RegimeGauge {
    previous_high: f64,
    previous_low: f64,
    previous_close: f64,
    range: Wilder,
    up_move: Wilder,
    down_move: Wilder,
    adx: Wilder,
    atr: Atr,
    atr_mean: Sma,
}

impl RegimeGauge {
    fn new() -> Self {
        RegimeGauge {
            previous_high: f64::NAN,
            previous_low: f64::NAN,
            previous_close: f64::NAN,
            range: Wilder::new(SMOOTHING),
            up_move: Wilder::new(SMOOTHING),
            down_move: Wilder::new(SMOOTHING),
            adx: Wilder::new(SMOOTHING),
            atr: Atr::new(SMOOTHING as f64).expect("a valid period"),
            atr_mean: Sma::new((SMOOTHING * 3) as f64).expect("a valid period"),
        }
    }

    /// Whether this bar is regime 1, then +DI and -DI.
    fn step(&mut self, bar: &Bar) -> (bool, f64, f64) {
        // `f64::max` drops a NaN, so the first bar's true range is its own span.
        let span = (bar.high - bar.low)
            .max((bar.high - self.previous_close).abs())
            .max((bar.low - self.previous_close).abs());
        // Negative moves floor at zero but an unknown one stays NaN, which `f64::max` would not keep. On the first bar both are
        // NaN, the comparison below fails, and only the up move becomes a known 0: the down line starts a bar late.
        let floored = |v: f64| if v < 0.0 { 0.0 } else { v };
        let mut up = floored(bar.high - self.previous_high);
        let mut down = floored(self.previous_low - bar.low);
        if up > down {
            down = 0.0;
        } else {
            up = 0.0;
        }
        (self.previous_high, self.previous_low, self.previous_close) =
            (bar.high, bar.low, bar.close);

        let range = self.range.push(span);
        let up = self.up_move.push(up);
        let down = self.down_move.push(down);
        // Every comparison here is false on a NaN and so takes the 0 or 1.0 side, as does the DI sum on the bar one line is known.
        let plus_di = if range > 0.0 { 100.0 * up / range } else { 0.0 };
        let minus_di = if range > 0.0 { 100.0 * down / range } else { 0.0 };
        let di_total = plus_di + minus_di;
        let dx = if di_total > 0.0 { 100.0 * (plus_di - minus_di).abs() / di_total } else { 0.0 };
        let adx = self.adx.push(dx);

        let atr = self.atr.next(bar);
        let atr_mean = self.atr_mean.next_value(atr);
        let atr_ratio = if atr_mean > 0.0 { atr / atr_mean } else { 1.0 };

        let regime = if atr_ratio >= WIDE_ATR && adx < TREND_ADX {
            3
        } else if adx >= TREND_ADX {
            1
        } else if atr_ratio <= NARROW_ATR {
            2
        } else {
            0
        };
        (regime == 1, plus_di, minus_di)
    }
}

const MIDDLE: f64 = 0.5;
const BAND: f64 = 0.05;

const CHANGE_BARS: usize = 10;

/// The twice-averaged close position and its change, copied from `pressure_gauge`.
struct PressureChain {
    first: Ema,
    second: Ema,
    recent: [f64; CHANGE_BARS + 1],
}

impl PressureChain {
    fn new() -> Self {
        PressureChain {
            first: Ema::new(14.0).expect("a valid period"),
            // The second average starts on the first's first value: the first's warm-up NaNs are skipped, not pushed.
            second: Ema::new(5.0).expect("a valid period").with_skip_nan(true),
            recent: [f64::NAN; CHANGE_BARS + 1],
        }
    }
    /// The averaged position, then its change over `CHANGE_BARS`. A bar with no range sits in the middle.
    fn step(&mut self, bar: &Bar) -> (f64, f64) {
        let range = bar.high - bar.low;
        let position = if range > 0.0 { (bar.close - bar.low) / range } else { MIDDLE };
        let first = self.first.next_value(position);
        let second = self.second.next_value(first);
        self.recent.rotate_left(1);
        self.recent[CHANGE_BARS] = second;
        (second, second - self.recent[0])
    }
}

const RSI_BULL: f64 = 55.0;
const RSI_BEAR: f64 = 45.0;
const RSI_LOOKBACK: usize = 3;
const VOTES_NEEDED: u8 = 2;

/// The two vote counts, copied from `rsi_macd_momentum`.
struct MomentumVotes {
    rsi: IndicatorLine,
    macd: IndicatorLine,
    recent_rsi: [f64; RSI_LOOKBACK + 1],
    previous_hist: f64,
}

impl MomentumVotes {
    fn new() -> Self {
        MomentumVotes {
            rsi: IndicatorLine::new("rsi", &[14.0]),
            macd: IndicatorLine::new("macd", &[12.0, 26.0, 9.0]),
            recent_rsi: [f64::NAN; RSI_LOOKBACK + 1],
            previous_hist: f64::NAN,
        }
    }
    /// The bull votes, then the bear votes.
    fn step(&mut self, bar: &Bar) -> (u8, u8) {
        let rsi = self.rsi.next(bar)[0];
        self.recent_rsi.rotate_left(1);
        self.recent_rsi[RSI_LOOKBACK] = rsi;
        let then = self.recent_rsi[0];
        // The `macd` row's lines are macd, signal, hist.
        let hist = self.macd.next(bar)[2];
        let previous_hist = std::mem::replace(&mut self.previous_hist, hist);

        let bull = [rsi > RSI_BULL, rsi > then, hist > 0.0 && hist > previous_hist];
        let bear = [rsi < RSI_BEAR, rsi < then, hist < 0.0 && hist < previous_hist];
        let count = |votes: [bool; 3]| votes.iter().filter(|&&v| v).count() as u8;
        (count(bull), count(bear))
    }
}

const ORDER_POINTS: u8 = 30;
const STRUCTURE_POINTS: u8 = 20;
const BIAS_TREND_POINTS: u8 = 20;
const BIAS_POINTS: u8 = 30;

/// The score of one side. A NaN average earns nothing.
fn points(in_order: bool, beyond_structure: bool, beyond_trend: bool) -> u8 {
    u8::from(in_order) * ORDER_POINTS
        + u8::from(beyond_structure) * STRUCTURE_POINTS
        + u8::from(beyond_trend) * BIAS_TREND_POINTS
}

/// Whether each side has bias, copied from `three_ema_bias_score`.
struct BiasScore {
    averages: [Ema; 3],
}

impl BiasScore {
    fn new() -> Self {
        BiasScore { averages: [21.0, 55.0, 200.0].map(|n| Ema::new(n).expect("a valid period")) }
    }
    /// Bullish bias, then bearish bias.
    fn step(&mut self, bar: &Bar) -> (bool, bool) {
        let [fast, slow, trend] = [0, 1, 2].map(|i| self.averages[i].next(bar));
        let close = bar.close;
        let bull = points(fast > slow && slow > trend, close > fast && close > slow, close > trend);
        let bear = points(fast < slow && slow < trend, close < fast && close < slow, close < trend);
        (bull >= BIAS_POINTS, bear >= BIAS_POINTS)
    }
}
