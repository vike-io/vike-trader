// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "dca" by lukescream, https://www.tradingview.com/script/mycmqNjC/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2023-01-27, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Buys 100 USDT of BTC, floored to the step, on the first bar of each month's 15th, every buy a lot of its own, and closes
//! them all on the day the rule runs. Orders fill at the close that placed them. No costs; version 5 starts with 1,000,000.
//!
//! The rule reads the clock of its run (`timenow`), the run's last bar (`vike_tactics::Tactics::window`): every lot closes at
//! the first close of that bar's UTC day. A run with no window (a paper mount) has no last day, so it closes nothing. Its daily
//! call, with gaps, feeds nothing that trades, and its exit on a gain is off.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! fill = "close"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_marketdata::Bar;
use vike_model::MS_PER_DAY;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::calendar::day_of_month;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "DCA";
const BUY_DAY: i64 = 15;
const SPEND: f64 = 100.0;

struct S {
    day: Option<i64>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S { day: None })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { pyramiding: 10_000, ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let day = bar.timestamp.div_euclid(MS_PER_DAY);
        let new_day = self.day.is_some_and(|d| d != day);
        self.day = Some(day);
        if new_day && day_of_month(bar.timestamp) == BUY_DAY {
            strategy.entry(NAME, Long).size(sizing.floor(SPEND / bar.close));
        }
        if strategy.window().is_some_and(|(_, last)| last.div_euclid(MS_PER_DAY) == day) {
            strategy.close(NAME);
        }
    }
}
