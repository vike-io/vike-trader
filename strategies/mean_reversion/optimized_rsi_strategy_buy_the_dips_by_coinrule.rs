// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Optimized RSI Strategy - Buy The Dips (by Coinrule)" by Coinrule, https://www.tradingview.com/script/Pm1WAtyI/
// The original's notice: © Coinrule
// Changed: rewritten by hand in Rust for vike.

//! Long 30% of the account's worth when RSI(14) is under 35 and the close is under its 100-bar SMA; closed when RSI(14) is
//! over 65. 0.1% commission; starts with 1,000.
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
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const NAME: &str = "long";
const ENTER: f64 = 35.0;
const EXIT: f64 = 65.0;

struct S {
    rsi: IndicatorLine,
    average: IndicatorLine,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        rsi: IndicatorLine::new("rsi", &[14.0]),
        average: IndicatorLine::new("sma", &[100.0]),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.1), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let rsi = self.rsi.next(bar)[0];
        let average = self.average.next(bar)[0];
        if rsi < ENTER && bar.close < average {
            strategy.entry(NAME, Long).size(share_of_worth(&sizing, 30.0, bar.close));
        }
        if rsi > EXIT {
            strategy.close(NAME);
        }
    }
}
