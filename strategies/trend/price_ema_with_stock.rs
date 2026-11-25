// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Price EMA with stock" by SoftKill21, https://www.tradingview.com/script/IS7yz9s4/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2020-12-07, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! EMA(50) is near the price when a close of the last five bars crossed it, or it crossed the low or the high of one of the four
//! bars before this one. Then long 1 when the close is above EMA(50), EMA(50) above EMA(100), the 3-bar mean of the 5-bar
//! stochastic crosses up through 20 and the MACD (12, 26, 9) histogram is not negative; short 1 on the mirror (the stochastic
//! crossing down through 80), each reversing the other. Each bar with EMA(50) crossing one of those lows or highs sets a target
//! and a stop 10% of this close from the entry, both ways, for the trade held or the next one of each side. No costs; starts
//! with 100,000.
//!
//! The 10% is written as a count of ticks, `close * 0.1 / tick`, and the level as the entry plus that count times the tick.
//! A cross either way is `rolling::CrossEither`.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_tactics::helpers::{back, push};
use vike_tactics::rolling::CrossEither;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

use vike_indicators::indicators::{Ema, Stochastic};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;

const UP: &str = "long";
const DOWN: &str = "short";
const SHARE: f64 = 0.1;

struct S {
    fifty: IndicatorLine,
    hundred: IndicatorLine,
    stoch: Stochastic,
    k_low: Cross,
    k_high: Cross,
    fast: IndicatorLine,
    slow: IndicatorLine,
    signal: Ema,
    /// Close, low and high, newest first, six of each.
    closes: VecDeque<f64>,
    lows: VecDeque<f64>,
    highs: VecDeque<f64>,
    /// The close `j` bars back against EMA(50), `j` from 0 to 4.
    close_cross: [Cross; 5],
    either_four: CrossEither,
    low_touch: [CrossEither; 4],
    high_touch: [CrossEither; 4],
    /// The distance a target and stop lie from the entry, for each side, once a bar has set it.
    long_exit: Option<f64>,
    short_exit: Option<f64>,
    side: i32,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fifty: IndicatorLine::new("ema", &[50.0]),
        hundred: IndicatorLine::new("ema", &[100.0]),
        stoch: Stochastic::new(5.0, 3.0, 1.0).expect("valid lengths").with_fresh_sum(true),
        k_low: Cross::new(),
        k_high: Cross::new(),
        fast: IndicatorLine::new("ema", &[12.0]),
        slow: IndicatorLine::new("ema", &[26.0]),
        signal: Ema::new(9.0).expect("a valid length").with_skip_nan(true),
        closes: VecDeque::with_capacity(7),
        lows: VecDeque::with_capacity(7),
        highs: VecDeque::with_capacity(7),
        close_cross: std::array::from_fn(|_| Cross::new()),
        either_four: CrossEither::default(),
        low_touch: Default::default(),
        high_touch: Default::default(),
        long_exit: None,
        short_exit: None,
        side: 0,
    })
}

impl S {
    /// A side's target and stop, from the average cost of what is held.
    fn protect(&self, strategy: &mut Tactics<'_>) {
        let held = strategy.position();
        let cost = strategy.avg_cost();
        if let (true, Some(d)) = (held > 0.0, self.long_exit) {
            strategy.exit("x long").from(UP).take(cost + d).stop(cost - d);
        } else if let (true, Some(d)) = (held < 0.0, self.short_exit) {
            strategy.exit("x short").from(DOWN).take(cost - d).stop(cost + d);
        }
    }

    /// A side that closed forgets its exit.
    fn follow(&mut self, strategy: &Tactics<'_>) {
        let held = strategy.position();
        if self.side > 0 && held <= 0.0 {
            self.long_exit = None;
        }
        if self.side < 0 && held >= 0.0 {
            self.short_exit = None;
        }
        self.side = if held > 0.0 {
            i32::from(Long)
        } else if held < 0.0 {
            i32::from(Short)
        } else {
            0
        };
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        self.follow(strategy);
        let fifty = self.fifty.next(bar)[0];
        let hundred = self.hundred.next(bar)[0];
        let k = self.stoch.next(bar)[0];
        let k_up = matches!(self.k_low.step(k, 20.0), Signal::Long);
        let k_down = matches!(self.k_high.step(k, 80.0), Signal::Short);
        let macd = self.fast.next(bar)[0] - self.slow.next(bar)[0];
        let hist = macd - self.signal.next_value(macd);
        push(&mut self.closes, bar.close, 6);
        push(&mut self.lows, bar.low, 6);
        push(&mut self.highs, bar.high, 6);
        let mut over = false;
        let mut under = false;
        for (j, cross) in self.close_cross.iter_mut().enumerate() {
            match cross.step(back(&self.closes, j), fifty) {
                Signal::Long => over = true,
                Signal::Short if j < 4 => under = true,
                _ => {}
            }
        }
        under |= self.either_four.step(back(&self.closes, 4), fifty);
        let mut touch = false;
        for j in 1..=4 {
            touch |= self.low_touch[j - 1].step(back(&self.lows, j), fifty);
            touch |= self.high_touch[j - 1].step(back(&self.highs, j), fifty);
        }
        let near = over || under || touch;
        let c = bar.close;
        if near && c > fifty && k_up && fifty > hundred && hist >= 0.0 {
            strategy.entry(UP, Long).size(1.0);
        }
        if near && c < fifty && k_down && fifty < hundred && hist <= 0.0 {
            strategy.entry(DOWN, Short).size(1.0);
        }
        if touch {
            let d = c * SHARE / tick * tick;
            (self.long_exit, self.short_exit) = (Some(d), Some(d));
            self.protect(strategy);
        }
    }

    // The exits' levels are measured from the entry's fill, so they are issued here, after it; under the simulator's price
    // order (`[engine.in_bar] price_order`) they work from the fill's own level on.
    fn after_fill(&mut self, strategy: &mut Tactics<'_>, _bar: &Bar) {
        self.follow(strategy);
        self.protect(strategy);
    }
}
