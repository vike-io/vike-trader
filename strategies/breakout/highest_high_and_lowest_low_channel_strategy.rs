// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Highest High and Lowest Low Channel Strategy" by ceyhun, https://www.tradingview.com/script/1SMJivyk/
// The original's notice: © ceyhun
// Changed: rewritten by hand in Rust for vike.

//! Long 1 once the close is above the previous bar's 20-bar highest high, short 1 once it is below the previous bar's 20-bar
//! lowest low, holding the last side in between and reversing from the other. No costs; starts with 1,000,000.
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
    channel: Donchian,
    /// The previous bar's `[highest, mid, lowest]`.
    last: [f64; 3],
    side: i32,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        channel: Donchian::new(20.0).expect("a valid length"),
        last: [f64::NAN; 3],
        side: 0,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let [highest, _, lowest] = self.last;
        if bar.close > highest {
            self.side = i32::from(Long);
        } else if bar.close < lowest {
            self.side = i32::from(Short);
        }
        self.last = self.channel.next(bar);
        match self.side {
            1 => {
                strategy.entry("long", Long).size(1.0);
            }
            -1 => {
                strategy.entry("short", Short).size(1.0);
            }
            _ => {}
        }
    }
}
