// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Hull Suite Strategy" by DashTrader, https://www.tradingview.com/script/Q9OQye4C/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2020-05-25, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! HMA(55) of the close against its value two bars back. Long all of the account's worth while the average is above it;
//! the short entry while it is below is a side the rule may not enter (`strategy.risk.allow_entry_in` long only), so it
//! closes the long instead. No costs; starts with 100,000.
//!
//! The average's half length is 55 / 2 in whole numbers, 27, and its last stage `round(sqrt(55))`, 7: vike's `Hma`.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Hma;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "buy";

struct S {
    hma: Hma,
    /// The average one and two bars back.
    last: [f64; 2],
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S { hma: Hma::new(55.0).expect("a valid length"), last: [f64::NAN; 2] })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let hull = self.hma.next_value(bar.close);
        let back = self.last[1];
        if hull > back {
            strategy.entry(NAME, Long).size(share_of_worth(&sizing, 100.0, bar.close));
        }
        if hull < back {
            strategy.close(NAME);
        }
        self.last = [hull, self.last[0]];
    }
}
