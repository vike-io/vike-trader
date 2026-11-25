// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "RSI Trend Crypto" by FieryTrading, https://www.tradingview.com/script/Ru7qOVtp/
// The original's notice: © FieryTrading
// Changed: rewritten by hand in Rust for vike.

//! Long all of the account's worth when RSI(14) crosses up through 35; closed when it crosses down through 75. The rule's
//! emergency exit is off by default. 0.2% commission; starts with 100,000.
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
use vike_model::Commission;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "long";
const ENTER: f64 = 35.0;
const EXIT: f64 = 75.0;

struct S {
    rsi: IndicatorLine,
    enter: Cross,
    exit: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S { rsi: IndicatorLine::new("rsi", &[14.0]), enter: Cross::new(), exit: Cross::new() })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.2), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let rsi = self.rsi.next(bar)[0];
        if matches!(self.enter.step(rsi, ENTER), Signal::Long) {
            strategy.entry(NAME, Long).size(share_of_worth(&sizing, 100.0, bar.close));
        }
        if matches!(self.exit.step(rsi, EXIT), Signal::Short) {
            strategy.close(NAME);
        }
    }
}
