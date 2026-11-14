// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Super Scalper - 5 Min 15 Min" by VinayKumarKV, https://www.tradingview.com/script/4ORGCiTh/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2022-03-22, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Bands one 14-bar WMA of the true range either side of the close. Long 1 when the open is below the lower band and RSI(25)
//! is above RSI(100); short 1 when the open is above the upper band and RSI(25) is below RSI(100); each reversing the other.
//! No costs; starts with 1,000,000.
//!
//! The rule works out a stop and a target on each signal and never places them.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Rsi, TrueRange, Wma};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

struct S {
    range: TrueRange,
    width: Wma,
    fast: Rsi,
    slow: Rsi,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        range: TrueRange::default(),
        width: Wma::new(14.0).expect("a valid length"),
        fast: Rsi::new(25.0).expect("a valid length"),
        slow: Rsi::new(100.0).expect("a valid length"),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let width = self.width.next_value(self.range.next(bar));
        let (fast, slow) = (self.fast.next(bar), self.slow.next(bar));
        let upper = width + bar.close;
        let lower = bar.close - width;
        if bar.open < lower && fast > slow {
            strategy.entry("long", Long).size(1.0);
        }
        if bar.open > upper && fast < slow {
            strategy.entry("short", Short).size(1.0);
        }
    }
}
