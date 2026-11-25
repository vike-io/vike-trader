// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "BTC 15 min" by RafaelZioni, https://www.tradingview.com/script/xGk5K4DE/
// The original's notice: © RafaelZioni
// Taken into the original, as it says: two lines (the first close of each day) from "security-free MTF example - JD" by Duyck, https://www.tradingview.com/script/Ql1FjjfX-security-free-MTF-example-JD/
// That script states no licence: published on 2020-05-26, it is MPL-2.0 by the Terms of Use of the site that publishes it.
// Changed: rewritten by hand in Rust for vike.

//! Two signals feed a pair of counters that let each through once until the other side's signal. The long signal: the
//! negated 10-bar EMA of a volume oscillator crosses up through 0, or the 25-bar linear regression of the close crosses
//! up through a Hull-like line of 1-minute closes. The oscillator is the RSI(1) of the running sum of each UTC day's
//! first close's change times that bar's volume, less its 10-bar WMA, both a bar back. The short signal: the regression
//! crosses down through the 1-minute line, or the last close crosses down through the SMA(30) of the close plus 5% of the
//! close. A long signal let through notes its bar's open; while the last signal let through was a long, a high crossing
//! up through 2% over that open is the take-profit signal (let through once until the next long signal), and a low
//! crossing down through 5% under it the rebuy signal.
//!
//! Orders: a long when a long signal is let through with EMA(3) of the close over EMA(21), or on the rebuy signal; a short
//! on the take-profit signal. Each entry is the account's equity over this bar's open, rounded to 4 decimals, and reverses
//! the other side. Every bar a position is held, two exits each guard half of its entry: a target 1% (the first) and 2%
//! (the second) of the average price away, both with a stop 5% away, each a whole number of ticks from the entry's
//! price. Commission 0.075%; starts with 10,000. The direction allowed is "all", which limits nothing. The channel's
//! bands, the Fibonacci levels of past entries and the colours are drawn only, and no order reads them.
//!
//! The 1-minute line is `wma(3 * wma(close, 8) - wma(close, 12) - wma(close, 25), 25)` (the header's integer halves of
//! 50), a bar back: an input of the run's own symbol at that bar length (decision 0271), read at the chart bar's close,
//! over its bars from the rule's first bar on, as it stood one 1-minute bar before the last of them inside the chart bar.
//! The day's first close is read off the chart's own bars (the original takes this reading from another published
//! example, which it cites by its page).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;

use vike_indicators::indicators::{Ema, Linearreg, Sma, Wma};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::higher::HigherTimeframeSourceUnseen;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::{floor_printed, floor_share};
use vike_tactics::{Rule, RuleDefaults, Tactics, TacticsInput};

const ONE_MINUTE: &str = "1m";
const DAY_MS: i64 = 86_400_000;
const CHANNEL_LENGTH: f64 = 30.0;
const CHANNEL_PERCENT: f64 = 5.0;
const OSCILLATOR_LENGTH: usize = 10;
const REGRESSION_LENGTH: f64 = 25.0;
const TAKE_PROFIT_PERCENT: f64 = 2.0;
const REBUY_PERCENT: f64 = 5.0;
const EXIT_STOP_PERCENT: f64 = 5.0;
const EXIT_TARGETS: [(&str, f64); 2] = [("x1", 1.0), ("x2", 2.0)];
const EXIT_SHARE: f64 = 50.0;

/// The 1-minute line, fed each bar of its input once, from the rule's first bar on.
struct Lower {
    unseen: HigherTimeframeSourceUnseen,
    eighth: Wma,
    twelfth: Wma,
    half: Wma,
    line: Wma,
    now: f64,
    before: f64,
}

impl Lower {
    fn new() -> Self {
        let wma = |length: f64| Wma::new(length).expect("a valid length");
        Lower {
            unseen: HigherTimeframeSourceUnseen::new(),
            eighth: wma(8.0),
            twelfth: wma(12.0),
            half: wma(25.0),
            line: wma(25.0),
            now: f64::NAN,
            before: f64::NAN,
        }
    }

    /// The line one 1-minute bar before the last of `input` closed by now, its bars before `from` left out.
    fn next(&mut self, input: TacticsInput<'_>, from: i64) -> f64 {
        let bars = self.unseen.next(input);
        for bar in &bars[bars.partition_point(|b| b.timestamp < from)..] {
            let inner = 3.0 * self.eighth.next(bar) - self.twelfth.next(bar) - self.half.next(bar);
            self.before = self.now;
            self.now = self.line.next_value(inner);
        }
        self.before
    }
}

/// A 10-bar weighted average summed afresh each bar, so a window of equal values gives that value exactly; `NaN` until
/// ten values and while one of them is missing.
struct ExactWma {
    window: VecDeque<f64>,
}

impl ExactWma {
    fn next(&mut self, x: f64) -> f64 {
        self.window.push_back(x);
        if self.window.len() > OSCILLATOR_LENGTH {
            self.window.pop_front();
        }
        if self.window.len() < OSCILLATOR_LENGTH {
            return f64::NAN;
        }
        let (mut sum, mut norm) = (0.0, 0.0);
        for (i, value) in self.window.iter().rev().enumerate() {
            let weight = ((OSCILLATOR_LENGTH - i) * OSCILLATOR_LENGTH) as f64;
            norm += weight;
            sum += value * weight;
        }
        sum / norm
    }
}

struct S {
    /// The rule's first bar's open: no 1-minute bar before it is fed.
    from: Option<i64>,
    lower: Lower,
    basis: Sma,
    fast: Ema,
    slow: Ema,
    regression: Linearreg,
    regression_cross: Cross,
    oscillator_cross: Cross,
    /// The UTC day of the bar before, the day's first close and the running sum.
    day_before: Option<i64>,
    day_close: f64,
    running: f64,
    running_before: Option<f64>,
    strength: ExactWma,
    strength_before: f64,
    average_before: f64,
    smooth: Ema,
    /// The last close and the band as the bar before and the one before it read them.
    close_before: [f64; 2],
    upper_before: f64,
    /// The counters of let-through signals, the long pair and the take-profit pair.
    longs: u32,
    shorts: u32,
    longs_after: u32,
    profits: u32,
    long_open: f64,
    long_time: i64,
    short_time: i64,
    high_before: f64,
    low_before: f64,
    profit_level_before: f64,
    rebuy_level_before: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        from: None,
        lower: Lower::new(),
        basis: Sma::new(CHANNEL_LENGTH).expect("a valid length"),
        fast: Ema::new(3.0).expect("a valid length"),
        slow: Ema::new(21.0).expect("a valid length"),
        regression: Linearreg::new(REGRESSION_LENGTH).expect("a valid length"),
        regression_cross: Cross::new(),
        oscillator_cross: Cross::new(),
        day_before: None,
        day_close: f64::NAN,
        running: 0.0,
        running_before: None,
        strength: ExactWma { window: VecDeque::new() },
        strength_before: f64::NAN,
        average_before: f64::NAN,
        smooth: Ema::new(OSCILLATOR_LENGTH as f64).expect("a valid length").with_skip_nan(true),
        close_before: [f64::NAN; 2],
        upper_before: f64::NAN,
        longs: 0,
        shorts: 0,
        longs_after: 0,
        profits: 0,
        long_open: 0.0,
        long_time: 0,
        short_time: 0,
        high_before: f64::NAN,
        low_before: f64::NAN,
        profit_level_before: f64::NAN,
        rebuy_level_before: f64::NAN,
    })
}

impl S {
    /// The oscillator's EMA, negated, after this bar.
    fn oscillator(&mut self, bar: &Bar) -> f64 {
        let day = bar.timestamp.div_euclid(DAY_MS);
        let day_close_before = self.day_close;
        if self.day_before.is_some_and(|before| before != day) {
            self.day_close = bar.close;
        }
        self.day_before = Some(day);
        let change = (self.day_close - day_close_before) * bar.volume;
        if change.is_finite() {
            self.running += change;
        }
        // The RSI of one bar: 100 unless the sum fell (no move reads 100), none on the first bar.
        let strength = match self.running_before {
            None => f64::NAN,
            Some(before) if self.running < before => 0.0,
            Some(_) => 100.0,
        };
        self.running_before = Some(self.running);
        let average = self.strength.next(strength);
        let gap = self.strength_before - self.average_before;
        (self.strength_before, self.average_before) = (strength, average);
        -self.smooth.next_value(gap)
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.075), ..RuleDefaults::PLAIN }
    }

    fn inputs(&self) -> Vec<String> {
        vec![ONE_MINUTE.to_string()]
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let from = *self.from.get_or_insert(bar.timestamp);
        let line = self.lower.next(strategy.input(ONE_MINUTE), from);
        let close = bar.close;
        let upper = self.basis.next(bar) + CHANNEL_PERCENT * close / 100.0;
        let fast = self.fast.next(bar);
        let slow = self.slow.next(bar);
        let regression = self.regression.next_value(close);
        let regression_cross = self.regression_cross.step(regression, line);
        let oscillator = self.oscillator(bar);
        let oscillator_up = matches!(self.oscillator_cross.step(oscillator, 0.0), Signal::Long);

        let [close_1, close_2] = self.close_before;
        let close_under_band = close_1 < upper && close_2 >= self.upper_before;
        self.close_before = [close, close_1];
        self.upper_before = upper;

        let long_signal = oscillator_up || matches!(regression_cross, Signal::Long);
        let short_signal = matches!(regression_cross, Signal::Short) || close_under_band;
        if long_signal {
            self.longs += 1;
            self.shorts = 0;
        }
        if short_signal {
            self.longs = 0;
            self.shorts += 1;
        }
        let long_through = long_signal && self.longs <= 1;
        let short_through = short_signal && self.shorts <= 1;
        if long_through {
            self.long_open = bar.open;
            self.long_time = bar.timestamp;
        }
        if short_through {
            self.short_time = bar.timestamp;
        }
        let in_long = self.long_time > self.short_time;
        let profit_level = (1.0 + TAKE_PROFIT_PERCENT / 100.0) * self.long_open;
        let rebuy_level = (1.0 - REBUY_PERCENT / 100.0) * self.long_open;
        let take_profit = bar.high > profit_level
            && self.high_before <= self.profit_level_before
            && !long_through
            && in_long;
        let rebuy = bar.low < rebuy_level
            && self.low_before >= self.rebuy_level_before
            && !long_through
            && in_long;
        (self.high_before, self.low_before) = (bar.high, bar.low);
        (self.profit_level_before, self.rebuy_level_before) = (profit_level, rebuy_level);
        if long_through {
            self.longs_after += 1;
            self.profits = 0;
        }
        if take_profit {
            self.longs_after = 0;
            self.profits += 1;
        }
        let profit_through = take_profit && self.profits <= 1;

        let sizing = strategy.sizing();
        let size = floor_printed(
            (strategy.equity() / bar.open * 10_000.0).round() / 10_000.0,
            sizing.qty_step(),
        );
        if (fast > slow && long_through) || rebuy {
            strategy.entry("buy", Long).size(size);
        }
        if profit_through {
            strategy.entry("sell", Short).size(size);
        }

        let average = strategy.avg_cost();
        let Some(tick) =
            strategy.tick().filter(|_| strategy.position() != 0.0 && !average.is_nan())
        else {
            return;
        };
        let ticks = |percent: f64| (percent / 100.0 * average / tick).round() * tick;
        let held: Vec<_> = strategy.bindings().iter().filter(|b| b.left > 0.0).cloned().collect();
        for binding in held {
            let away = if binding.side == Long { 1.0 } else { -1.0 };
            let stop = binding.price - away * ticks(EXIT_STOP_PERCENT);
            let part =
                floor_share(binding.size * EXIT_SHARE / 100.0, binding.size, sizing.qty_step());
            for (name, percent) in EXIT_TARGETS {
                strategy
                    .exit(name)
                    .from(&binding.name)
                    .size(part)
                    .take(binding.price + away * ticks(percent))
                    .stop(stop);
            }
        }
    }
}
