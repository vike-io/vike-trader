// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Grab Trading System" by LonesomeTheBlue, https://www.tradingview.com/script/1HAKnNRc/
// The original's notice: © LonesomeTheBlue
// Changed: rewritten by hand in Rust for vike.

//! The trend turns up when the high passes the 80-bar highest high as it stood on the bar before, down when the low passes
//! the 80-bar lowest low likewise. Long 1 while it is up, on a bar whose low is above the 21-bar lowest low after a bar whose
//! low was that bar's 21-bar lowest; short 1 while it is down on the mirror, each reversing the other. While long: a stop at
//! the 80-bar lowest low, moved on every bar, and a close at the next open on the bar that would signal a short; while short
//! the mirror. Any position closes at the next open on the bar the trend changes. No costs; starts with 100,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Donchian;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const BUY: &str = "Buy";
const SELL: &str = "Sell";

struct S {
    major: Donchian,
    minor: Donchian,
    trend: i32,
    /// The previous bar: `(high, low, major high, major low, minor high, minor low)`.
    previous: Option<(f64, f64, f64, f64, f64, f64)>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        major: Donchian::new(80.0).expect("a valid length"),
        minor: Donchian::new(21.0).expect("a valid length"),
        trend: 0,
        previous: None,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let [major_high, _, major_low] = self.major.next(bar);
        let [minor_high, _, minor_low] = self.minor.next(bar);
        let now = (bar.high, bar.low, major_high, major_low, minor_high, minor_low);
        let Some((high1, low1, major_high1, major_low1, minor_high1, minor_low1)) =
            self.previous.replace(now)
        else {
            return;
        };
        let was = self.trend;
        if bar.high > major_high1 {
            self.trend = 1;
        } else if bar.low < major_low1 {
            self.trend = -1;
        }
        let low_turn = low1 == minor_low1 && bar.low > minor_low;
        let high_turn = high1 == minor_high1 && bar.high < minor_high;
        if self.trend == 1 && low_turn {
            strategy.entry(BUY, Long).size(1.0);
        }
        if self.trend == -1 && high_turn {
            strategy.entry(SELL, Short).size(1.0);
        }
        let held = strategy.position();
        if held > 0.0 {
            strategy.exit("stop buy").from(BUY).stop(major_low);
            if high_turn {
                strategy.close(BUY);
            }
        }
        if held < 0.0 {
            strategy.exit("stop sell").from(SELL).stop(major_high);
            if low_turn {
                strategy.close(SELL);
            }
        }
        if held != 0.0 && self.trend != was {
            strategy.close_all();
        }
    }
}
