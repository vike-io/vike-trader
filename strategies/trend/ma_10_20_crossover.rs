// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "MA 10/20 Crossover" by BigBeluga, https://www.tradingview.com/script/2cbpO8lO/
// The original's notice: © HunterAlgos
// Changed: rewritten by hand in Rust for vike.

//! Long 1 when SMA(10) of the close crosses up through SMA(20), short 1 when it crosses down, each reversing the other, on the
//! bars from 2016-04-01 to 2018-10-15 (the rule's date window; the daily bars on file start in 2017). No costs; starts with
//! 1,000,000.
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
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::calendar::timestamp;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

struct S {
    fast: Sma,
    slow: Sma,
    cross: Cross,
    from: i64,
    to: i64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: Sma::new(10.0).expect("a valid length"),
        slow: Sma::new(20.0).expect("a valid length"),
        cross: Cross::new(),
        from: timestamp(2016, 4, 1, 0, 0),
        to: timestamp(2018, 10, 15, 0, 0),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let signal = self.cross.step(self.fast.next(bar), self.slow.next(bar));
        if bar.timestamp < self.from || bar.timestamp > self.to {
            return;
        }
        match signal {
            Signal::Long => {
                strategy.entry("Long", Long).size(1.0);
            }
            Signal::Short => {
                strategy.entry("Short", Short).size(1.0);
            }
            _ => {}
        }
    }
}
