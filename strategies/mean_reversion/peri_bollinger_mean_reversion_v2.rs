// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Peri Bollinger Mean Reversion V2" by thechadyogi, https://www.tradingview.com/script/XRPeqEdA/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2025-12-06, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Bollinger bands of the close (SMA(20) plus and minus 2 population standard deviations) with RSI(14). A plain order (it adds to
//! or reduces whatever is held) of `units` buys when the close is under the lower band with RSI under 30 and less than 5 times
//! `units` is held, and `units` sells when the close is over the upper band with RSI over 70 and less than 5 times `units` is
//! short. `units` is the whole number of coins 10% of the start plus the profit made buys. Longs close when the close is over
//! the upper band with RSI over 50, shorts when it is at or under the middle band. No costs; starts with 100,000 under a 100%
//! margin. A plain order is `vike_tactics::Tactics::order`.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Bollinger, Rsi};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const CASH: f64 = 100_000.0;

struct S {
    bands: Bollinger,
    rsi: Rsi,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        bands: Bollinger::new(20.0, 2.0).expect("valid parameters"),
        rsi: Rsi::new(14.0).expect("a valid length").with_rs_form(true),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let [upper, basis, lower] = self.bands.next(bar);
        let rsi = self.rsi.next_value(bar.close);
        let units = (0.1 * (CASH + strategy.closed_net().unwrap_or(0.0)) / bar.close).floor();
        let held = strategy.position();
        if bar.close < lower && rsi < 30.0 && held < units * 5.0 {
            strategy.order("Long", Long).size(units);
        }
        if bar.close > upper && rsi > 50.0 && held > 0.0 {
            strategy.close("Long");
        }
        if bar.close > upper && rsi > 70.0 && held > -units * 5.0 {
            strategy.order("Short", Short).size(units);
        }
        if held < 0.0 && bar.close <= basis {
            strategy.close("Short");
        }
    }
}
