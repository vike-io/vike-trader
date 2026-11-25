// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Wyckoff Range Strategy by deperp" by deperp, https://www.tradingview.com/script/vQSBf9rh/
// The original's notice: © deperp
// Changed: rewritten by hand in Rust for vike.

//! Long 10% of the account's worth when the close crosses up through SMA(32) on the bar the low crosses up through its SMA(10);
//! short 10% when the close crosses down through SMA(35) on the bar the high crosses down through its SMA(20), each closing
//! the other side first. On every bar each side's stop is moved to 10% past this close. No costs; starts with 1,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Sma;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const STOP: f64 = 0.10;
const SHARE: f64 = 10.0;

struct S {
    accumulation: IndicatorLine,
    distribution: IndicatorLine,
    spring: Sma,
    upthrust: Sma,
    accumulation_cross: Cross,
    distribution_cross: Cross,
    spring_cross: Cross,
    upthrust_cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        accumulation: IndicatorLine::new("sma", &[32.0]),
        distribution: IndicatorLine::new("sma", &[35.0]),
        spring: Sma::new(10.0).expect("a valid length"),
        upthrust: Sma::new(20.0).expect("a valid length"),
        accumulation_cross: Cross::new(),
        distribution_cross: Cross::new(),
        spring_cross: Cross::new(),
        upthrust_cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let accumulation = self.accumulation.next(bar)[0];
        let distribution = self.distribution.next(bar)[0];
        let spring = self.spring.next_value(bar.low);
        let upthrust = self.upthrust.next_value(bar.high);
        let accumulating =
            matches!(self.accumulation_cross.step(bar.close, accumulation), Signal::Long);
        let distributing =
            matches!(self.distribution_cross.step(bar.close, distribution), Signal::Short);
        let sprung = matches!(self.spring_cross.step(bar.low, spring), Signal::Long);
        let thrust = matches!(self.upthrust_cross.step(bar.high, upthrust), Signal::Short);
        if accumulating && sprung {
            strategy.close("short");
            strategy.entry("long", Long).size(share_of_worth(&sizing, SHARE, bar.close));
        }
        if distributing && thrust {
            strategy.close("long");
            strategy.entry("short", Short).size(share_of_worth(&sizing, SHARE, bar.close));
        }
        let cut = bar.close * (1.0 - STOP);
        strategy.exit("long stop").from("long").stop(cut);
        let cut = bar.close * (1.0 + STOP);
        strategy.exit("short stop").from("short").stop(cut);
    }
}
