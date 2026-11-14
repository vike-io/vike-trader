// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "MTF Screener MA Cross System" by yigdeli, https://www.tradingview.com/script/SUKIoh1K/
// The original's notice: © yigdeli
// Changed: rewritten by hand in Rust for vike.

//! A cross of EMA(9) through EMA(27) filtered by the trend of the last closed 4-hour bar. Long 10% of the account's worth on a cross
//! up with the close over the Hull average (200) of the 4-hour closes, short on a cross down with the close under it, each reversing
//! the other. Once a position is held at a bar's close, a stop 2% and a target 4% from its average price rest from the next bar on
//! (the "Basic" management mode, in percent). No costs; starts with 10,000.
//!
//! The rule also asks the same cross of ten other symbols for a table it draws; none of it reaches an order.
//!
//! The 4-hour bars are built from the run's own symbol's 30-minute bars from the rule's first bar on
//! (`vike_tactics::higher::HigherTimeframeSourceIntraday`).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Ema, Hma};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::higher::{self, HigherTimeframeSourceIntraday};
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const STOP_PERCENT: f64 = 2.0;
const TARGET_PERCENT: f64 = 4.0;

struct S {
    fours: HigherTimeframeSourceIntraday,
    trend: Hma,
    /// The Hull average of the last closed 4-hour bar.
    line: f64,
    fast: Ema,
    medium: Ema,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fours: HigherTimeframeSourceIntraday::new(240),
        trend: Hma::new(200.0).expect("a valid length"),
        line: f64::NAN,
        fast: Ema::new(9.0).expect("a valid length"),
        medium: Ema::new(27.0).expect("a valid length"),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn inputs(&self) -> Vec<String> {
        vec![higher::HALF_HOUR.to_string()]
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        for four in self.fours.next(bar, strategy.input(higher::HALF_HOUR)) {
            self.line = self.trend.next_value(four.close);
        }
        let cross = self.cross.step(self.fast.next(bar), self.medium.next(bar));
        match cross {
            Signal::Long if bar.close > self.line => {
                strategy.entry("Long", Long).size(share_of_worth(&sizing, 10.0, bar.close));
            }
            Signal::Short if bar.close < self.line => {
                strategy.entry("Short", Short).size(share_of_worth(&sizing, 10.0, bar.close));
            }
            _ => {}
        }
        let held = strategy.position();
        let average = strategy.avg_cost();
        let (stop, target) = (STOP_PERCENT / 100.0, TARGET_PERCENT / 100.0);
        if held > 0.0 {
            strategy
                .exit("Long Exit")
                .from("Long")
                .take(average * (1.0 + target))
                .stop(average * (1.0 - stop));
        }
        if held < 0.0 {
            strategy
                .exit("Short Exit")
                .from("Short")
                .take(average * (1.0 - target))
                .stop(average * (1.0 + stop));
        }
    }
}
