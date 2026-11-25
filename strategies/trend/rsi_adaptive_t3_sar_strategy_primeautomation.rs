// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "RSI-Adaptive T3 & SAR Strategy [PrimeAutomation]" by ChartPrime, https://www.tradingview.com/script/bldXp6gY/
// The original's notice: © PrimeAutomation
// Changed: rewritten by hand in Rust for vike.

//! A T3 average whose length follows RSI(14) of the close: `round(5 + 45 * (1 - rsi / 100))`, a high RSI giving a short
//! length. Six chained averages `e1` to `e6` (each `alpha * x + (1 - alpha) * last`, `alpha = 2 / (length + 1)`, started at
//! its input where it has no value before), volume factor 0.7, `t3 = c1 * e6 + c2 * e5 + c3 * e4 + c4 * e3`. The line crosses
//! its value two bars back: long 10% of the account's worth on a cross up with the close above the parabolic SAR (0.02 start,
//! 0.0002 step, 0.21 cap) and no long held, short on a cross down with the close not above the SAR and no short held; the
//! opposite position is closed first. A stop at the lowest low (the highest high) of the last 10 bars on the signal bar guards
//! the entry. Orders fill at the close of the bar that placed them. 0.06% commission; starts with 10,000; the margins are
//! stated as 0, so there is no margin call. **Does not reproduce its reference** (472 trades, net -583.517395 on BTCUSDT
//! 30m, 2025-01-01..2026-07-30): this file gives 470 / -575.047750, two trades fewer and the net 8.47 over: every trade from
//! 2025-01-04 10:00 on is the same under each reading of the T3 chain's early state, the reference has two more closed in
//! the first three days, and some 40 readings of that state reach neither; the owner accepted those numbers on 2026-11-16.
//!
//! The RSI is the reference's: Wilder averages of the moves, the plain mean of the first 14 seeding them, and
//! `100 - 100 / (1 + up / down)` (vike's `Rsi` in the RS form). A first average whose input is missing starts the chain
//! again: while the RSI has no value the length has none, the averages alternate between their input and nothing, and the
//! first valid length picks up whichever it finds. The parabolic SAR is written below because vike's `Psar` takes no step
//! under 0.001 and this header's is 0.0002; the header imports a library of the platform's but calls only the SAR, which
//! that library does not define. The rule's 5-minute call feeds only an exit before the market closes, which its input
//! leaves off, so no order reads a lower timeframe; the alert texts, the daily profit list and the plots are left out.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! fill = "close"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Donchian, Rsi};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const VOLUME: f64 = 0.7;
const STOP_LENGTH: f64 = 10.0;
const SHARE: f64 = 10.0;

/// The rule's own average: `alpha * src + (1 - alpha) * last`, or `src` where the last value is missing.
fn chained(last: f64, src: f64, length: f64) -> f64 {
    let alpha = 2.0 / (length + 1.0);
    if last.is_nan() { src } else { alpha * src + (1.0 - alpha) * last }
}

/// The platform's parabolic SAR giving this bar's value: started on the second bar from the first two closes, a reversal
/// judged on the bar the SAR reaches, the factor raised only on a bar that is not a reversal, the SAR then kept beyond the
/// last two bars.
struct Sar {
    start: f64,
    increment: f64,
    maximum: f64,
    bars: usize,
    up: bool,
    extreme: f64,
    sar: f64,
    factor: f64,
    /// `(high, low, close)` one and two bars back.
    last: [(f64, f64, f64); 2],
}

impl Sar {
    fn new(start: f64, increment: f64, maximum: f64) -> Self {
        let none = (f64::NAN, f64::NAN, f64::NAN);
        Sar {
            start,
            increment,
            maximum,
            bars: 0,
            up: false,
            extreme: f64::NAN,
            sar: f64::NAN,
            factor: start,
            last: [none, none],
        }
    }

    /// This bar's SAR, NaN on the first bar.
    fn next(&mut self, bar: &Bar) -> f64 {
        let (high, low) = (bar.high, bar.low);
        let mut result = f64::NAN;
        if self.bars > 0 {
            let (high1, low1, close1) = self.last[0];
            let mut first = false;
            if self.bars == 1 {
                if bar.close > close1 {
                    (self.up, self.extreme, self.sar) = (true, high, low1);
                } else {
                    (self.up, self.extreme, self.sar) = (false, low, high1);
                }
                first = true;
                self.factor = self.start;
            }
            self.sar += self.factor * (self.extreme - self.sar);
            if self.up && self.sar > low {
                first = true;
                self.up = false;
                self.sar = self.extreme.max(high);
                self.extreme = low;
                self.factor = self.start;
            } else if !self.up && self.sar < high {
                first = true;
                self.up = true;
                self.sar = self.extreme.min(low);
                self.extreme = high;
                self.factor = self.start;
            }
            if !first {
                if self.up && high > self.extreme {
                    self.extreme = high;
                    self.factor = (self.factor + self.increment).min(self.maximum);
                } else if !self.up && low < self.extreme {
                    self.extreme = low;
                    self.factor = (self.factor + self.increment).min(self.maximum);
                }
            }
            let (high2, low2, _) = self.last[1];
            if self.up {
                self.sar = self.sar.min(low1);
                if self.bars > 1 {
                    self.sar = self.sar.min(low2);
                }
            } else {
                self.sar = self.sar.max(high1);
                if self.bars > 1 {
                    self.sar = self.sar.max(high2);
                }
            }
            result = self.sar;
        }
        self.last = [(high, low, bar.close), self.last[0]];
        self.bars += 1;
        result
    }
}

struct S {
    rsi: Rsi,
    averages: [f64; 6],
    sar: Sar,
    channel: Donchian,
    /// The line at 1, 2 and 3 bars back.
    t3: [f64; 3],
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        rsi: Rsi::new(14.0).expect("a valid length").with_rs_form(true),
        averages: [f64::NAN; 6],
        sar: Sar::new(0.02, 0.0002, 0.21),
        channel: Donchian::new(STOP_LENGTH).expect("a valid length"),
        t3: [f64::NAN; 3],
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.06), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let rsi = self.rsi.next_value(bar.close);
        let length = (5.0 + 45.0 * (1.0 - rsi / 100.0)).round();
        let mut input = bar.close;
        for average in &mut self.averages {
            *average = chained(*average, input, length);
            input = *average;
        }
        let [_, _, e3, e4, e5, e6] = self.averages;
        let v = VOLUME;
        let c1 = -v * v * v;
        let c2 = 3.0 * v * v + 3.0 * v * v * v;
        let c3 = -6.0 * v * v - 3.0 * v - 3.0 * v * v * v;
        let c4 = 1.0 + 3.0 * v + v * v * v + 3.0 * v * v;
        let t3 = c1 * e6 + c2 * e5 + c3 * e4 + c4 * e3;
        let [t3_1, t3_2, t3_3] = self.t3;
        let up = t3 > t3_2 && t3_1 <= t3_3;
        let down = t3 < t3_2 && t3_1 >= t3_3;
        self.t3 = [t3, t3_1, t3_2];

        let sar = self.sar.next(bar);
        let [stop_high, _, stop_low] = self.channel.next(bar);
        let above = bar.close > sar;
        let held = strategy.position();
        let size = share_of_worth(&strategy.sizing(), SHARE, bar.close);
        if up && above && held <= 0.0 {
            strategy.close("Short");
            strategy.entry("Long", Long).size(size);
            strategy.exit("Long Stop").from("Long").stop(stop_low);
        }
        if down && !above && held >= 0.0 {
            strategy.close("Long");
            strategy.entry("Short", Short).size(size);
            strategy.exit("Short Stop").from("Short").stop(stop_high);
        }
    }
}
