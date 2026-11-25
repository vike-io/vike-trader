// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Noro's TrendMA Strategy" by ROBO_Trading, https://www.tradingview.com/script/BRDFaufy/
// The original's notice: © noro
// Changed: rewritten by hand in Rust for vike.

//! The trend is up when SMA(10) and SMA(30) of the bar's four-price mean both rose on the bar, down when both fell, and
//! otherwise what it was. Long all of the account's worth while it is up, short all of it while it is down, each reversing the
//! other. 0.1% commission; starts with 100,000.
//!
//! A rise is read as `average > average one bar back`, so while either average is still warming up it reads as a fall: the
//! trend is down from the first bar.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Sma;
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

struct S {
    fast: Sma,
    slow: Sma,
    fast_before: f64,
    slow_before: f64,
    /// `1` up, `-1` down, `0` not yet either.
    trend: i32,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: Sma::new(10.0).expect("a valid length"),
        slow: Sma::new(30.0).expect("a valid length"),
        fast_before: f64::NAN,
        slow_before: f64::NAN,
        trend: 0,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.1), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let price = (bar.open + bar.high + bar.low + bar.close) / 4.0;
        let fast = self.fast.next_value(price);
        let slow = self.slow.next_value(price);
        let fast_up = fast > self.fast_before;
        let slow_up = slow > self.slow_before;
        if fast_up && slow_up {
            self.trend = 1;
        } else if !fast_up && !slow_up {
            self.trend = -1;
        }
        (self.fast_before, self.slow_before) = (fast, slow);
        match self.trend {
            1 => {
                strategy.entry("Long", Long).size(share_of_worth(&sizing, 100.0, bar.close));
            }
            -1 => {
                strategy.entry("Short", Short).size(share_of_worth(&sizing, 100.0, bar.close));
            }
            _ => {}
        }
    }
}
