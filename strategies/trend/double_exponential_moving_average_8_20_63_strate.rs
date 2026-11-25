// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Double Exponential Moving Average 8-20-63 Strategy" by Noldo, https://www.tradingview.com/script/AbRhwDeX/
// The original's notice: © Noldo
// Changed: rewritten by hand in Rust for vike.

//! Three double EMAs (`2 * EMA - EMA of that EMA`): 8 bars of the four-price mean, 20 of the low, 63 of the close. Long all of
//! the account's worth while the first is above both others, short all of it while below both, each reversing the other. 0.1%
//! commission; starts with 100,000.
//!
//! Each is vike's `Dema`, its inner EMA started on the outer one's first number.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Dema;
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

struct S {
    short: Dema,
    long: Dema,
    longer: Dema,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let dema = |n: f64| Dema::new(n).expect("a valid length");
    Box::new(S { short: dema(8.0), long: dema(20.0), longer: dema(63.0) })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.1), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let one = self.short.next_value((bar.open + bar.high + bar.low + bar.close) / 4.0);
        let two = self.long.next_value(bar.low);
        let three = self.longer.next(bar);
        if one > two && one > three {
            strategy.entry("Long", Long).size(share_of_worth(&sizing, 100.0, bar.close));
        }
        if one < two && one < three {
            strategy.entry("Short", Short).size(share_of_worth(&sizing, 100.0, bar.close));
        }
    }
}
