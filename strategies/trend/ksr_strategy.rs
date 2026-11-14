// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "KSR Strategy" by greadandfear, https://www.tradingview.com/script/N2c1PkLs/
// The original's notice: © askkuldeeprandhawa
// Changed: rewritten by hand in Rust for vike.

//! Long 1 when EMA(10) of the close crosses up through EMA(20), short 1 when it crosses down, each reversing the other. No
//! costs; starts with 100,000.
//!
//! The rule also reads the Heikin-Ashi candles of a day and a week, but only to colour its chart: nothing it trades on.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Ema;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

struct S {
    fast: Ema,
    slow: Ema,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: Ema::new(10.0).expect("a valid length"),
        slow: Ema::new(20.0).expect("a valid length"),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let (fast, slow) = (self.fast.next(bar), self.slow.next(bar));
        match self.cross.step(fast, slow) {
            Signal::Long => {
                strategy.entry("My Long Entry Id", Long).size(1.0);
            }
            Signal::Short => {
                strategy.entry("My Short Entry Id", Short).size(1.0);
            }
            _ => {}
        }
    }
}
