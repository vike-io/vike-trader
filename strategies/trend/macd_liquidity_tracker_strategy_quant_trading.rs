// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "MACD Liquidity Tracker Strategy [Quant Trading]" by Quant_Trading_Pro, https://www.tradingview.com/script/7ix2EvNm/
// The original's notice: © LiquidityTrckr
// Changed: rewritten by hand in Rust for vike.

//! MACD(25, 60, 220) of the close. Long all of the account's worth on the first bar the line is above its signal line; closed
//! on the first bar it is below. 0.1% commission, 3 ticks of slippage; starts with 1,000.
//!
//! The rule's "Normal" system, with shorts, stops and targets off as its inputs set them. Its margin of 0 makes no margin call,
//! and its date range (2018 to 2069) holds on every bar.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Macd;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const NAME: &str = "Long";

struct S {
    macd: Macd,
    was_above: bool,
    was_below: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        macd: Macd::new(25.0, 60.0, 220.0).expect("valid lengths"),
        was_above: false,
        was_below: false,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.1), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let [line, signal, _] = self.macd.next(bar);
        let above = line > signal;
        let below = line < signal;
        if above && !self.was_above {
            strategy.entry(NAME, Long).size(share_of_worth(
                &sizing,
                100.0,
                sizing.slipped(Long, bar.close),
            ));
        }
        if below && !self.was_below {
            strategy.close(NAME);
        }
        (self.was_above, self.was_below) = (above, below);
    }
}
