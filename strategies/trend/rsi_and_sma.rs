// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "RSI and SMA" by Coinrule, https://www.tradingview.com/script/Clpg5ju2/
// The original's notice: © Coinrule
// Changed: rewritten by hand in Rust for vike.

//! Long 30% of the account's worth when the 100-bar SMA of the close crosses up through the 150-bar one with RSI(14) above
//! 50, short 30% when it crosses down with RSI(14) below 50, each reversing the other. Orders fill at the signal bar's close.
//! 0.1% commission; starts with 1,000.
//!
//! The rule's two closes name an order id that no entry uses, so they close nothing.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! fill = "close"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Rsi, Sma};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const SHARE: f64 = 30.0;
const MIDDLE: f64 = 50.0;

struct S {
    rsi: Rsi,
    fast: Sma,
    slow: Sma,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        rsi: Rsi::new(14.0).expect("a valid length"),
        fast: Sma::new(100.0).expect("a valid length"),
        slow: Sma::new(150.0).expect("a valid length"),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.1), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let rsi = self.rsi.next(bar);
        let fast = self.fast.next(bar);
        let slow = self.slow.next(bar);
        match self.cross.step(fast, slow) {
            Signal::Long if rsi > MIDDLE => {
                strategy.entry("long", Long).size(share_of_worth(&sizing, SHARE, bar.close));
            }
            Signal::Short if rsi < MIDDLE => {
                strategy.entry("short", Short).size(share_of_worth(&sizing, SHARE, bar.close));
            }
            _ => {}
        }
    }
}
