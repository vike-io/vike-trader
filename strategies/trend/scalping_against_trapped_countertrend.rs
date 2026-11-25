// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "scalping against trapped countertrend" by clocks156t174, https://www.tradingview.com/script/mEJH90Q5/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2021-01-28, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! A pullback inside a trend, on hl2. Long 1 BTC on every bar where all three of these are above 0: the 13-bar SMA of the
//! 60-bar stochastic of hl2 (against the highs and lows) less 50, the 5-bar RSI of hl2 less 50, and 35 less the lowest of that
//! RSI over 15 bars. Short 1 BTC on every bar where all three of the mirror are below 0: the same two less 50, and 65 less the
//! highest of that RSI over 15 bars. Each reverses the other. No costs; starts with 100,000.
//!
//! The rule's least and greatest of three are undefined while any of the three is, which `strategy_kit::min` and `strategy_kit::max`
//! keep.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Donchian, Rsi, Stochastic};
use vike_indicators::strategy_kit::{max, min};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

struct S {
    stochastic: Stochastic,
    rsi: Rsi,
    range: Donchian,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        stochastic: Stochastic::new(60.0, 13.0, 1.0).expect("valid lengths"),
        rsi: Rsi::new(5.0).expect("a valid length"),
        range: Donchian::new(15.0).expect("a valid length"),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let hl2 = (bar.high + bar.low) / 2.0;
        let [k, _] = self.stochastic.next_hlc(bar.high, bar.low, hl2);
        let rsi = self.rsi.next_value(hl2);
        let [highest, _, lowest] = self.range.next_hl(rsi, rsi);
        if min(min(k - 50.0, rsi - 50.0), 35.0 - lowest) > 0.0 {
            strategy.entry("long", Long).size(1.0);
        }
        if max(max(k - 50.0, rsi - 50.0), 65.0 - highest) < 0.0 {
            strategy.entry("short", Short).size(1.0);
        }
    }
}
