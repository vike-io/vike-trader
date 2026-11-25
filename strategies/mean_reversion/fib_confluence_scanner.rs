// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Fib Confluence Scanner" by blitz_locked, https://www.tradingview.com/script/rKsnPJbJ/
// The original's notice: © blitzlocked
// Changed: rewritten by hand in Rust for vike.

//! Fifteen retracement levels: 23.6, 38.2, 50, 61.8 and 78.6% of the way down from the highest high toward the lowest low of
//! the last 20, 50 and 100 bars. Flat, with the close within 0.5% of any of them: long all of the account's worth at the next
//! open if the close is above EMA(50), short it if below. Held, a stop 1.5% and a target 3% from the average cost, issued on
//! every bar the position is held. 0.05% commission; version 6: starts with 10,000, margined at 100% (`main.rs`'s rule 4).
//!
//! A lot cut by the margin call and still open on the last bar has paid its whole entry commission into the trades already
//! closed (the lab's `whole_entry_fee`): the reference's net, to the cent, only so.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Donchian, Ema};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const RATIOS: [f64; 5] = [0.236, 0.382, 0.5, 0.618, 0.786];
const TOLERANCE: f64 = 0.5 / 100.0;
const STOP: f64 = 1.5 / 100.0;
const REWARD: f64 = 2.0;

struct S {
    ranges: [Donchian; 3],
    trend: Ema,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let range = |n: f64| Donchian::new(n).expect("a valid length");
    Box::new(S {
        ranges: [range(20.0), range(50.0), range(100.0)],
        trend: Ema::new(50.0).expect("a valid length"),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let close = bar.close;
        let mut near = 0;
        for range in &mut self.ranges {
            let [hi, _, lo] = range.next(bar);
            let span = hi - lo;
            near += RATIOS
                .iter()
                .filter(|&&r| (close - (hi - r * span)).abs() / close <= TOLERANCE)
                .count();
        }
        let ema = self.trend.next(bar);
        let held = strategy.position();
        if near >= 1 && held == 0.0 {
            let size = share_of_worth(&strategy.sizing(), 100.0, close);
            if close > ema {
                strategy.entry("Long", Long).size(size);
            } else if close < ema {
                strategy.entry("Short", Short).size(size);
            }
        }
        let cost = strategy.avg_cost();
        if held > 0.0 {
            strategy
                .exit("Exit Long")
                .from("Long")
                .stop(cost * (1.0 - STOP))
                .take(cost * (1.0 + STOP * REWARD));
        } else if held < 0.0 {
            strategy
                .exit("Exit Short")
                .from("Short")
                .stop(cost * (1.0 + STOP))
                .take(cost * (1.0 - STOP * REWARD));
        }
    }
}
