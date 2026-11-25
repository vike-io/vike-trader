// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Turtle System" by racer8, https://www.tradingview.com/script/6TnqZSoe/
// The original's notice: © racer8
// Changed: rewritten by hand in Rust for vike.

//! Breakouts: long 1 when the close goes above the highest high of the 55 bars before it, short 1 when it goes below their
//! lowest low, each reversing the other. A long is closed when the low goes below the lowest low of the 20 bars before it, a
//! short when the high goes above their highest high. No costs; starts with 100,000.
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

struct S {
    entry: Donchian,
    exit: Donchian,
    /// Last bar's `[upper, mid, lower]` of each channel.
    entry_before: [f64; 3],
    exit_before: [f64; 3],
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        entry: Donchian::new(55.0).expect("a valid length"),
        exit: Donchian::new(20.0).expect("a valid length"),
        entry_before: [f64::NAN; 3],
        exit_before: [f64::NAN; 3],
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let [high_entry, _, low_entry] = self.entry_before;
        let [high_exit, _, low_exit] = self.exit_before;
        if bar.close > high_entry {
            strategy.entry("Buy", Long).size(1.0);
        }
        if bar.close < low_entry {
            strategy.entry("Sell", Short).size(1.0);
        }
        if bar.low < low_exit {
            strategy.close("Buy");
        }
        if bar.high > high_exit {
            strategy.close("Sell");
        }
        self.entry_before = self.entry.next(bar);
        self.exit_before = self.exit.next(bar);
    }
}
