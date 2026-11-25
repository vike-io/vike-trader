// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "HIGHER HIGH LOWER LOW STRATEGY" by Craig_Claussen, https://www.tradingview.com/script/JWsBrWBu/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2019-12-10, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Long the whole account's worth when the close breaks above the highest high of the 41 bars before it and that high is
//! above the highest high of the 10 bars before it too (it is older than them); short the mirror on the lows. Each entry
//! reverses the other. No costs; starts with 100,000 in a USD account.
//!
//! The rule recalculates after each of its fills (`calc_on_order_fills`), which changes nothing here: its entries are placed
//! on the bar's close and the fill finds the side held, which the one-lot cap refuses.
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
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

struct S {
    wide: Donchian,
    guard: Donchian,
    /// The last bar's `[highest high, mid, lowest low]` of each.
    last: ([f64; 3], [f64; 3]),
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        wide: Donchian::new(41.0).expect("a valid length"),
        guard: Donchian::new(10.0).expect("a valid length"),
        last: ([f64::NAN; 3], [f64::NAN; 3]),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let ([highest, _, lowest], [guard_high, _, guard_low]) = self.last;
        if bar.close > highest && highest > guard_high {
            strategy.entry("Long", Long).size(share_of_worth(&sizing, 100.0, bar.close));
        }
        if bar.close < lowest && lowest < guard_low {
            strategy.entry("Short", Short).size(share_of_worth(&sizing, 100.0, bar.close));
        }
        self.last = (self.wide.next(bar), self.guard.next(bar));
    }
}
