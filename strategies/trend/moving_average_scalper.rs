// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Moving Average Scalper" by Coinrule, https://www.tradingview.com/script/PfBSgqMw/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2021-04-29, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Long 30% of the account's worth when SMA(9) crosses up through SMA(50) while SMA(50) < SMA(100) < SMA(200); closed when
//! SMA(9) crosses up through SMA(200). 0.1% commission; starts with 1,000.
//!
//! The rule's date window (2019-01-10 to 2112) holds on every bar here, and its stop and target are worked out but never used.
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

struct S {
    signal: IndicatorLine,
    fast: IndicatorLine,
    mid: IndicatorLine,
    slow: IndicatorLine,
    bullish: Cross,
    bearish: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        signal: IndicatorLine::new("sma", &[9.0]),
        fast: IndicatorLine::new("sma", &[50.0]),
        mid: IndicatorLine::new("sma", &[100.0]),
        slow: IndicatorLine::new("sma", &[200.0]),
        bullish: Cross::new(),
        bearish: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.1), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let signal = self.signal.next(bar)[0];
        let fast = self.fast.next(bar)[0];
        let mid = self.mid.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        let bullish = matches!(self.bullish.step(signal, fast), Signal::Long);
        let bearish = matches!(self.bearish.step(signal, slow), Signal::Long);
        if bullish && fast < mid && mid < slow {
            strategy.entry(NAME, Long).size(share_of_worth(&sizing, 30.0, bar.close));
        }
        if bearish {
            strategy.close(NAME);
        }
    }
}
