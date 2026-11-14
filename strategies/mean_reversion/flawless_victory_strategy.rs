// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Flawless Victory Strategy" by Trebor_Namor, https://www.tradingview.com/script/i3Uc79fF/
// The original's notice: © Bunghole
// Changed: rewritten by hand in Rust for vike.

//! Its first version, the one switched on: long all of the account's worth when the close is under the lower band of a 20-bar
//! Bollinger band of one standard deviation with RSI(14) above 42; closed when the close is over the upper band with RSI(14)
//! above 70. The other two versions and their stop and target are switched off. No costs; starts with 100,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "Long";

struct S {
    rsi: IndicatorLine,
    basis: IndicatorLine,
    deviation: IndicatorLine,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        rsi: IndicatorLine::new("rsi", &[14.0]),
        basis: IndicatorLine::new("sma", &[20.0]),
        deviation: IndicatorLine::new("stddev", &[20.0]),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let rsi = self.rsi.next(bar)[0];
        let basis = self.basis.next(bar)[0];
        let deviation = self.deviation.next(bar)[0];
        if bar.close < basis - deviation && rsi > 42.0 {
            strategy.entry(NAME, Long).size(share_of_worth(&sizing, 100.0, bar.close));
        }
        if bar.close > basis + deviation && rsi > 70.0 {
            strategy.close(NAME);
        }
    }
}
