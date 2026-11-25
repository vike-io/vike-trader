// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Pivot of Pivot Reversal Strategy [QuantNomad]" by QuantNomad, https://www.tradingview.com/script/mXygJWkv/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2019-12-09, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! A significant pivot high is a high two bars back that stands above each of the four bars before it and the two after it by
//! at least 0.1 ATR(14); a pivot low mirrors it. A pivot of pivots is the middle of the last three pivot highs when it is the
//! highest of them (the lowest of three lows for a low). A buy stop rests one tick above the latest pivot of pivot highs
//! while the latest pivot high has not been traded through, a sell stop one tick under the latest pivot of pivot lows
//! likewise, each for 1 and reversing the other. No costs; starts with 100,000.
//!
//! An order is issued again on every bar its condition holds, and rests where it was when the condition lapses. Issued again
//! on the side already held, it meets the pyramiding limit, which takes the order resting under its name off the book. While
//! the ATR warms up every comparison with it is false, so the first bars each make a pivot.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use vike_tactics::{Rule, RuleDefaults, Tactics};

use vike_indicators::indicators::Atr;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;

const LEFT: usize = 4;
const RIGHT: usize = 2;
const MULTIPLIER: f64 = 0.1;

/// One side's pivots: `dir` 1 for highs, -1 for lows, where a low is a high of the negated prices.
struct Side {
    dir: f64,
    /// The latest pivot, NaN before the first.
    price: f64,
    /// Not yet traded through since the latest pivot.
    armed: bool,
    /// The last three pivots, newest first, 0 before there are three.
    last: [f64; 3],
    /// The latest pivot of pivots, 0 before the first.
    pivot: f64,
}

impl Side {
    fn new(dir: f64) -> Self {
        Side { dir, price: f64::NAN, armed: false, last: [0.0; 3], pivot: 0.0 }
    }

    /// `values` newest first, as the rule reads the side's prices; `extreme` is this bar's high (low).
    fn step(&mut self, values: &VecDeque<f64>, atr: f64, extreme: f64) {
        let at = |k: usize| values.get(k).copied().unwrap_or(f64::NAN);
        let d = self.dir;
        let candidate = at(RIGHT);
        // NaN anywhere makes the comparison false, which keeps the pivot.
        let beaten = |k: usize| d * candidate < d * at(k) + atr * MULTIPLIER;
        let found = !(1..=LEFT).any(|i| beaten(RIGHT + i)) && !(0..RIGHT).any(beaten);
        let pivot = if found { candidate } else { f64::NAN };
        let cond = !pivot.is_nan();
        if cond {
            self.price = pivot;
        }
        // Before the first pivot the price is NaN, and nothing has traded through it.
        let through = d * extreme > d * self.price;
        self.armed = cond || (self.armed && !through);
        if cond {
            self.last = [self.price, self.last[0], self.last[1]];
            let [p1, p2, p3] = self.last;
            if d * p2 > d * p1 && d * p2 > d * p3 {
                self.pivot = p2;
            }
        }
    }
}

struct S {
    atr: Atr,
    highs: VecDeque<f64>,
    lows: VecDeque<f64>,
    up: Side,
    down: Side,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        atr: Atr::new(14.0).expect("a valid length"),
        highs: VecDeque::with_capacity(LEFT + RIGHT + 2),
        lows: VecDeque::with_capacity(LEFT + RIGHT + 2),
        up: Side::new(1.0),
        down: Side::new(-1.0),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let atr = self.atr.next(bar);
        for (values, v) in [(&mut self.highs, bar.high), (&mut self.lows, bar.low)] {
            values.push_front(v);
            values.truncate(LEFT + RIGHT + 1);
        }
        self.up.step(&self.highs, atr, bar.high);
        self.down.step(&self.lows, atr, bar.low);
        if self.up.armed && self.up.pivot != 0.0 {
            strategy.entry("PivRevLE", Long).size(1.0).stop(self.up.pivot + tick);
        }
        if self.down.armed && self.down.pivot != 0.0 {
            strategy.entry("PivRevSE", Short).size(1.0).stop(self.down.pivot - tick);
        }
    }
}
