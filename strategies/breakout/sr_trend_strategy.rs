// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "SR TREND STRATEGY" by pmk07, https://www.tradingview.com/script/hwHKYkE0/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2019-10-03, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Support and resistance from a five-bar swing: a high two bars back that two highs rise to and two fall from marks a
//! resistance (a low the mirror, a support), each read at the bar three back. Over 21 bars the mean level of the resistances and
//! of the supports sets a range `r`, the gap between those means; the last support lifted by `r` and the last resistance
//! lowered by `r` are the two lines the close must clear: above both is an up trend, below both a down trend, else the last
//! one holds. A trend turning down closes the long, turning up the short; while the trend is up and the close is over the last
//! resistance, long 1, while down and the close is under the last support, short 1, each reversing the other. No costs; starts
//! with 100,000.
//!
//! The rule recalculates after each of its orders fills, reading that bar's final values, so it looks ahead: an order that
//! fills at an open is followed by the rule as the bar's close will make it, and what it then orders fills at that same open.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use vike_tactics::helpers::{back, push};
use vike_tactics::rolling::RollingSum;
use vike_tactics::{Rule, RuleDefaults, Tactics};

use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;

const LENGTH: usize = 21;

struct S {
    index: usize,
    highs: VecDeque<f64>,
    lows: VecDeque<f64>,
    highs_n: RollingSum,
    lows_n: RollingSum,
    high_levels: RollingSum,
    low_levels: RollingSum,
    low_c: f64,
    high_c: f64,
    low_r: f64,
    high_r: f64,
    trend: i32,
    /// The bar the state has already advanced to: a fill inside a bar feeds the rule ahead of its close.
    fed: Option<i64>,
    /// The position the orders last issued leave: a rerun comes only once every fill of the pass has landed.
    want: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        index: 0,
        highs: VecDeque::new(),
        lows: VecDeque::new(),
        highs_n: RollingSum::new(LENGTH),
        lows_n: RollingSum::new(LENGTH),
        high_levels: RollingSum::new(LENGTH),
        low_levels: RollingSum::new(LENGTH),
        low_c: f64::NAN,
        high_c: f64::NAN,
        low_r: f64::NAN,
        high_r: f64::NAN,
        trend: 0,
        fed: None,
        want: 0.0,
    })
}

impl S {
    fn advance(&mut self, bar: &Bar) {
        push(&mut self.highs, bar.high, 6);
        push(&mut self.lows, bar.low, 6);
        let (h, l) = (|i| back(&self.highs, i), |i| back(&self.lows, i));
        let up = self.index > 5 && h(5) < h(4) && h(4) < h(3) && h(3) > h(2) && h(2) > h(1);
        let down = self.index > 5 && l(5) > l(4) && l(4) > l(3) && l(3) < l(2) && l(2) < l(1);
        self.index += 1;
        let hval = if up { h(3) } else { 0.0 };
        let lval = if down { l(3) } else { 0.0 };
        let ln = self.lows_n.next(f64::from(u8::from(down)));
        let hn = self.highs_n.next(f64::from(u8::from(up)));
        let lsum = self.low_levels.next(lval);
        let hsum = self.high_levels.next(hval);
        let r = if ln > 0.0 && hn > 0.0 { (hsum / hn - lsum / ln).abs() } else { 0.0 };
        if lval != 0.0 && r > 0.0 {
            self.low_c = lval;
            self.low_r = lval + r;
        }
        if hval != 0.0 && r > 0.0 {
            self.high_c = hval;
            self.high_r = hval - r;
        }
        let close = bar.close;
        if close > self.low_r && close > self.high_r {
            self.trend = 1;
        } else if close < self.low_r && close < self.high_r {
            self.trend = -1;
        }
    }

    fn rule(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        if self.fed != Some(bar.timestamp) {
            self.fed = Some(bar.timestamp);
            self.advance(bar);
        }
        let mut pos = strategy.position();
        if self.trend == -1 {
            strategy.close("Long");
            pos = pos.min(0.0);
        }
        if self.trend == 1 {
            strategy.close("Short");
            pos = pos.max(0.0);
        }
        if self.trend == 1 && bar.close > self.high_c {
            strategy.entry("Long", Long).size(1.0);
            pos = 1.0;
        }
        if self.trend == -1 && bar.close < self.low_c {
            strategy.entry("Short", Short).size(1.0);
            pos = -1.0;
        }
        self.want = pos;
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        self.rule(strategy, bar);
    }

    /// The reference recalculates once the fills of a pass are all in, so a fill the pass is still to follow waits.
    fn after_fill(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        if strategy.position() == self.want {
            self.rule(strategy, bar);
        }
    }
}
