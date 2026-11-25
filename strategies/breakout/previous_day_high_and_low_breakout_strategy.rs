// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Previous Day High and Low Breakout Strategy" by ceyhun, https://www.tradingview.com/script/dn6ogsKO/
// The original's notice: © ceyhun
// Changed: rewritten by hand in Rust for vike.

//! Long 1 when the high crosses up through the daily high the rule reads, short 1 when the low crosses down through its daily
//! low, each reversing the other. No costs; starts with 100,000.
//!
//! The rule reads `high[1]` and `low[1]` of the daily bar without lookahead: the day before the last one closed by this bar's
//! close, so two days back on a bar inside a day and the day before on the day's last bar. The days are the symbol's daily
//! input, its history before the window included (`vike_tactics::higher::HigherTimeframeSourceCalendar`).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::higher::{self, HigherTimeframeSourceCalendar};
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{HigherTimeframePeriod, Rule, RuleDefaults, Tactics};

struct S {
    days: HigherTimeframeSourceCalendar,
    /// `(high, low)` of the last day closed, and of the one before it.
    last: (f64, f64),
    before: (f64, f64),
    up: Cross,
    down: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        days: HigherTimeframeSourceCalendar::new(HigherTimeframePeriod::Day),
        last: (f64::NAN, f64::NAN),
        before: (f64::NAN, f64::NAN),
        up: Cross::new(),
        down: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn inputs(&self) -> Vec<String> {
        vec![higher::DAY.to_string()]
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        for day in self.days.next(bar, strategy.input(higher::DAY)) {
            self.before = self.last;
            self.last = (day.high, day.low);
        }
        let (high, low) = self.before;
        if matches!(self.up.step(bar.high, high), Signal::Long) {
            strategy.entry("Long", Long).size(1.0);
        }
        if matches!(self.down.step(bar.low, low), Signal::Short) {
            strategy.entry("Short", Short).size(1.0);
        }
    }
}
