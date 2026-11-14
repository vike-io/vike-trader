// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "X48 - Strategy | MA Multi Type Cross + TPSL + Swing + Surfing&Diving| Future&Spot | V.3.6" by X4815162342, https://www.tradingview.com/script/DAD06s8m/
// The original's notice: © X4815162342
// Changed: rewritten by hand in Rust for vike.

//! Long 20 worth when EMA(18) of the close crosses up through EMA(34), short 20 worth when it crosses down, each reversing
//! the other. 0.1% commission; starts with 200 in a USD account.
//!
//! The rule's target and stop are both 0, which it reads as none, so its exit order has no level and never fills. Its
//! monthly and 8-hour reads, its pivots and the imported library (X4815162342's `X48_LibaryStrategyStatus`, MPL-2.0) only
//! draw on its chart; the call on the chart's own timeframe is the close itself.
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
use vike_tactics::sizing::cash_worth;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const CASH: f64 = 20.0;

struct S {
    fast: Ema,
    slow: Ema,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: Ema::new(18.0).expect("a valid length"),
        slow: Ema::new(34.0).expect("a valid length"),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.1), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let (fast, slow) = (self.fast.next(bar), self.slow.next(bar));
        match self.cross.step(fast, slow) {
            Signal::Long => {
                strategy.entry("Long", Long).size(cash_worth(&sizing, CASH, bar.close));
            }
            Signal::Short => {
                strategy.entry("Short", Short).size(cash_worth(&sizing, CASH, bar.close));
            }
            _ => {}
        }
    }
}
