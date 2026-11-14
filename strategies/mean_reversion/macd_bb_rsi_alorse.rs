// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "MACD + BB + RSI [Alorse]" by alorse, https://www.tradingview.com/script/6xo9yMmT/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2021-09-15, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Long a fifth of the account's worth when the MACD (12, 26) crosses up through its 9-bar signal line with RSI(14) below 50 and
//! the close below the 20-bar Bollinger mean; closed when RSI(14) is above 70 with the close above the upper band (2 deviations).
//! The rule's stop is off by default and stays off. 0.01% commission; starts with 1,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Bollinger, Macd, Rsi};
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const NAME: &str = "long";

struct S {
    macd: Macd,
    bands: Bollinger,
    rsi: Rsi,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        macd: Macd::new(12.0, 26.0, 9.0).expect("valid lengths"),
        bands: Bollinger::new(20.0, 2.0).expect("valid parameters"),
        rsi: Rsi::new(14.0).expect("a valid length"),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.01), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let [macd, signal, _] = self.macd.next(bar);
        let [upper, basis, _] = self.bands.next(bar);
        let rsi = self.rsi.next(bar);
        let crossed = matches!(self.cross.step(macd, signal), Signal::Long);
        if crossed && rsi < 50.0 && bar.close < basis {
            strategy.entry(NAME, Long).size(share_of_worth(&sizing, 20.0, bar.close));
        }
        if rsi > 70.0 && bar.close > upper {
            strategy.close(NAME);
        }
    }
}
