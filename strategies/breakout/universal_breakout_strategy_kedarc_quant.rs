// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Universal Breakout Strategy [KedArc Quant]" by kedarcquant, https://www.tradingview.com/script/tSoUhlM9/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2025-10-01, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Long all of the account's worth when the close crosses up through the day before's high, short it when the close crosses
//! down through the day before's low, each reversing the other. No costs; version 6 with no capital: 100,000, margined at 100%.
//! The rule's other breakout modes are off.
//!
//! The rule asks for the daily bar with lookahead and reads its `[1]`: on every bar of a day that is the day before, which
//! had closed, so nothing it reads lies ahead. It evaluates on every tick; on a closed bar that is its close. The days are the
//! symbol's daily input, its history before the window included (`vike_tactics::higher::HigherTimeframeSourceCalendar`).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_marketdata::Bar;
use vike_model::MS_PER_DAY;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::higher::{self, HigherTimeframeSourceCalendar};
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{HigherTimeframePeriod, Rule, RuleDefaults, Tactics};

struct S {
    days: HigherTimeframeSourceCalendar,
    /// `(open time, high, low)` of the last day closed, and of the one before it.
    last: (i64, f64, f64),
    before: (i64, f64, f64),
    up: Cross,
    down: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        days: HigherTimeframeSourceCalendar::new(HigherTimeframePeriod::Day),
        last: (i64::MIN, f64::NAN, f64::NAN),
        before: (i64::MIN, f64::NAN, f64::NAN),
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
        let sizing = strategy.sizing();
        for day in self.days.next(bar, strategy.input(higher::DAY)) {
            self.before = self.last;
            self.last = (day.timestamp, day.high, day.low);
        }
        // On a day's last bar the day itself has just closed: the day before is the one before it.
        let today = bar.timestamp - bar.timestamp.rem_euclid(MS_PER_DAY);
        let (_, high, low) = if self.last.0 < today { self.last } else { self.before };
        if matches!(self.up.step(bar.close, high), Signal::Long) {
            strategy.entry("Long", Long).size(share_of_worth(&sizing, 100.0, bar.close));
        }
        if matches!(self.down.step(bar.close, low), Signal::Short) {
            strategy.entry("Short", Short).size(share_of_worth(&sizing, 100.0, bar.close));
        }
    }
}
