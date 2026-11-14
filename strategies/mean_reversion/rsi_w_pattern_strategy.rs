// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "RSI W Pattern strategy" by eemani123, https://www.tradingview.com/script/f80Cfpfp/
// The original's notice: © mohanee
// Changed: rewritten by hand in Rust for vike.

//! Long 1 when EMA(20) of the close is above EMA(50) and RSI(5) draws a W: up on this bar, down on the one before, up on the one
//! before that and down on the one before that, now between 30 and 65, with the low one or three bars back no higher than the
//! low four bars back. While held, one more lot (two at most) when RSI(5) crosses up through 10. Everything closes when RSI(5)
//! crosses up through 75 with the close above the average cost, or when the close falls more than 8% under it. No costs;
//! starts with 100,000.
//!
//! The RSI is vike's in the RS form (`Rsi::with_rs_form`): vike's unset rounds differently, which flips the W's comparisons
//! of two RSI values on the bars where they are nearly equal.
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
use vike_tactics::{Rule, RuleDefaults, Tactics};

use vike_indicators::indicators::Rsi;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;

const NAME: &str = "long";
const KEEP: usize = 5;

struct S {
    rsi: Rsi,
    short: IndicatorLine,
    long: IndicatorLine,
    /// RSI and the low, newest first, `KEEP` of each.
    rsis: VecDeque<f64>,
    lows: VecDeque<f64>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        rsi: Rsi::new(5.0).expect("a valid length").with_rs_form(true),
        short: IndicatorLine::new("ema", &[20.0]),
        long: IndicatorLine::new("ema", &[50.0]),
        rsis: VecDeque::with_capacity(KEEP + 1),
        lows: VecDeque::with_capacity(KEEP + 1),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { pyramiding: 2, ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        push(&mut self.rsis, self.rsi.next_value(bar.close), KEEP);
        push(&mut self.lows, bar.low, KEEP);
        let rising = self.short.next(bar)[0] > self.long.next(bar)[0];
        let r = |i| back(&self.rsis, i);
        let l = |i| back(&self.lows, i);
        let w = r(0) > r(1)
            && r(0) >= 30.0
            && r(1) < r(2)
            && r(2) > r(3)
            && r(3) < r(4)
            && (l(1) <= l(4) || l(3) <= l(4));
        if rising && w && r(0) <= 65.0 && r(0) >= 30.0 {
            strategy.entry(NAME, Long).size(1.0);
        }
        if strategy.position() > 0.0 && r(0) > 10.0 && r(1) <= 10.0 {
            strategy.entry(NAME, Long).size(1.0);
        }
        let cost = strategy.avg_cost();
        if r(0) > 75.0 && r(1) <= 75.0 && bar.close > cost {
            strategy.close(NAME);
        }
        if bar.close < cost - cost * 8.0 / 100.0 {
            strategy.close(NAME);
        }
    }
}
