// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Maximized Scalping On Trend (by Coinrule)" by Coinrule, https://www.tradingview.com/script/XAiEh7nb/
// The original's notice: © Coinrule
// Changed: rewritten by hand in Rust for vike.

//! Long 30% of the account's worth when the close crosses down through SMA(9) while SMA(50) is above SMA(100) and RSI(14) is
//! above 50; closed when RSI(14) is below 30 or above 70. Orders fill at the close of the bar that places them. 0.1%
//! commission; starts with 1,000.
//!
//! The rule's date range (2019 to 2112) holds on every bar here.
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
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "long";

struct S {
    fast: Sma,
    mid: Sma,
    slow: Sma,
    rsi: Rsi,
    dip: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: Sma::new(9.0).expect("a valid length"),
        mid: Sma::new(50.0).expect("a valid length"),
        slow: Sma::new(100.0).expect("a valid length"),
        rsi: Rsi::new(14.0).expect("a valid length"),
        dip: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.1), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let fast = self.fast.next(bar);
        let mid = self.mid.next(bar);
        let slow = self.slow.next(bar);
        let rsi = self.rsi.next(bar);
        let dip = matches!(self.dip.step(bar.close, fast), Signal::Short);
        if dip && mid > slow && rsi > 50.0 {
            strategy.entry(NAME, Long).size(share_of_worth(&sizing, 30.0, bar.close));
        }
        let take_profit = rsi > 70.0;
        let stop = rsi < 30.0;
        if stop || take_profit {
            strategy.close(NAME);
        }
    }
}
