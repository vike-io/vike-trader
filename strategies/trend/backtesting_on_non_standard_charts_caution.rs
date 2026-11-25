// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from the script by PineCoders, https://www.tradingview.com/script/q9laJNG9/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2019-11-11, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! A bar is up when its close, rounded to the tick, is over its open. Long all of the account's worth after two up bars that
//! followed a bar not up, judged one bar late (the bars one, two and three back); short all of it after two bars not up that
//! followed an up bar. The rule believes itself long or not: it enters long only when it is not, short only when it is, and
//! neither on the bar after an order the other way. Each entry reverses the other. No costs; starts with 100,000.
//!
//! The rule reads the chart's own prices through a call with lookahead on the chart's own timeframe, which is the bar
//! itself: nothing looks ahead. Its Heikin-Ashi reading is off.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;

const TICKS: f64 = 100.0;

struct S {
    /// Up or not, newest first: this bar, one back, two back, three back.
    up: VecDeque<bool>,
    in_long: bool,
    /// Last bar's orders, `(long, short)`.
    went: (bool, bool),
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S { up: VecDeque::new(), in_long: false, went: (false, false) })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let tick = |p: f64| (p * TICKS).round() / TICKS;
        self.up.push_front(tick(bar.close) > tick(bar.open));
        self.up.truncate(4);
        // A bar before the first reads as not up.
        let back = |i: usize| self.up.get(i).copied().unwrap_or(false);
        let (a, b, c) = (back(1), back(2), back(3));
        let (long, short) = (a && b && !c, !a && !b && c);
        let (went_long, went_short) = self.went;
        let mut go = (false, false);
        if !self.in_long && long && !went_short {
            go.0 = true;
            self.in_long = true;
        }
        if self.in_long && short && !went_long {
            go.1 = true;
            self.in_long = false;
        }
        if go.0 {
            strategy.entry("Long", Long).size(share_of_worth(&sizing, 100.0, bar.close));
        } else if go.1 {
            strategy.entry("Short", Short).size(share_of_worth(&sizing, 100.0, bar.close));
        }
        self.went = go;
    }
}
