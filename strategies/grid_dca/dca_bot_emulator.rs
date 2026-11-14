// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "DCA Bot Emulator" by ericlin0122, https://www.tradingview.com/script/vyStpg6g/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2021-03-29, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! A dollar-cost-averaging bot on the OHLC4 price. Flat, it buys 10 dollars' worth at that price. Each safety order after it
//! buys `10 * 1.1^(n-1)` dollars' worth once the price is down to `first * (1 - 2% * (1.1^n - 1) / 0.1)`, n counting from 1,
//! up to 30 of them. The whole position closes once the price reaches 1% above its average entry. 0.02% commission; starts
//! with 10,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::sizing::cash_worth;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const BASE: f64 = 10.0;
const DEVIATION: f64 = 2.0 / 100.0;
const TAKE_PROFIT: f64 = 1.0 / 100.0;
const SCALE: f64 = 1.1;
const MAX_SAFETY: i32 = 30;

struct S {
    /// The next safety order's number; 0 when no cycle is open.
    next: i32,
    first: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S { next: 0, first: 0.0 })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.02), pyramiding: 999, ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let price = (bar.open + bar.high + bar.low + bar.close) / 4.0;
        let target = strategy.avg_cost() * (1.0 + TAKE_PROFIT);
        if strategy.position() == 0.0 && price > 0.0 {
            strategy.entry("base", Long).size(cash_worth(&sizing, BASE, price));
            self.first = price;
            self.next = 1;
        }
        let n = f64::from(self.next);
        let level = self.first
            - (self.first * ((DEVIATION * libm::pow(SCALE, n) - DEVIATION) / (SCALE - 1.0)));
        if self.next > 0 && price <= level && self.next <= MAX_SAFETY {
            strategy.entry("safety", Long).size(cash_worth(
                &sizing,
                BASE * libm::pow(SCALE, n - 1.0),
                price,
            ));
            self.next += 1;
        }
        if target <= price && strategy.position() > 0.0 {
            strategy.close_all();
            self.next = 0;
        }
    }
}
