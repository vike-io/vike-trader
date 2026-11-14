// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "TSIHULLBOT" by SeaSide420, https://www.tradingview.com/script/gf21APTn/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2021-05-03, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! A true strength index of the open, smoothed by Hull averages: the bar-to-bar change of the open and its absolute value, each
//! through HMA(50) twice, give `100 * (change / absolute) * 5`, and the signal line is `HMA(7)` of that, times 5. Long all of the
//! account's worth when the signal line and the open both rose on this bar, short all of it when both fell, each reversing the
//! other. No costs; starts with 100,000.
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
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

struct S {
    change: [Hma; 2],
    absolute: [Hma; 2],
    signal: Hma,
    last_open: f64,
    last_line: f64,
}

fn hma(n: f64) -> Hma {
    Hma::new(n).expect("a valid length")
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        change: [hma(50.0), hma(50.0)],
        absolute: [hma(50.0), hma(50.0)],
        signal: hma(7.0),
        last_open: f64::NAN,
        last_line: f64::NAN,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let change = bar.open - self.last_open;
        let [c1, c2] = &mut self.change;
        let smoothed = c2.next_value(c1.next_value(change));
        let [a1, a2] = &mut self.absolute;
        let smoothed_abs = a2.next_value(a1.next_value(change.abs()));
        let tsi = 100.0 * (smoothed / smoothed_abs) * 5.0;
        let line = self.signal.next_value(tsi) * 5.0;
        let last_line = std::mem::replace(&mut self.last_line, line);
        let last_open = std::mem::replace(&mut self.last_open, bar.open);
        if line > last_line && bar.open > last_open {
            strategy.entry("long", Long).size(share_of_worth(&sizing, 100.0, bar.close));
        }
        if line < last_line && bar.open < last_open {
            strategy.entry("short", Short).size(share_of_worth(&sizing, 100.0, bar.close));
        }
    }
}
