// SPDX-License-Identifier: Apache-2.0 AND MPL-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-ies-adx-regime-classify-01/strategy.pine
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived in part from "Integrated Execution System [JOAT]", © officialjackofalltrades, https://www.tradingview.com/script/SxASLpF5-Integrated-Execution-System-JOAT/
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long only, on a four-way regime made from ADX(14) and ATR(14) over its own 42-bar mean. A wide ATR with a weak ADX is regime
//! 3, otherwise an ADX of 25 or more is regime 1, otherwise a narrow ATR is regime 2, and anything left is regime 0. Buy on the
//! bar regime 1 begins, close on any bar it does not hold.
//!
//! The ADX is written here because vike's `adx` row differs from this one three ways: on a tie between the two directional
//! moves one side keeps its move rather than both dropping to zero, the DI and DX lines read 0 rather than NaN while they warm
//! up, and that zero run is what seeds the ADX average.
//!
//! The ADX seed remembers its first bar.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::{Atr, Sma};
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

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

/// The regime reading, also copied into `combo_3`.
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

struct S {
    gauge: RegimeGauge,
    was_trending: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S { gauge: RegimeGauge::new(), was_trending: false }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let (trending, _, _) = self.gauge.step(bar);
        match (trending, std::mem::replace(&mut self.was_trending, trending)) {
            (true, false) => Signal::Long,
            (false, _) => Signal::Close,
            _ => Signal::Hold,
        }
    }
}
