// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Fixed Percent Stop Loss & Take Profit %" by theCrypster, https://www.tradingview.com/script/00WHGEoc/
// The original's notice: © theCrypster 2020
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2020-04-04, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Long 1 when EMA(5) crosses up through EMA(32), short 1 when it crosses down, each reversing the other. While held, on every
//! bar, a stop 5% and a target 10% from the average cost. No costs; starts with 100,000.
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
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const UP: &str = "long";
const DOWN: &str = "short";
const STOP: f64 = 0.05;
const TARGET: f64 = 0.10;

struct S {
    fast: IndicatorLine,
    slow: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: IndicatorLine::new("ema", &[5.0]),
        slow: IndicatorLine::new("ema", &[32.0]),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        match self.cross.step(fast, slow) {
            Signal::Long => {
                strategy.entry(UP, Long).size(1.0);
            }
            Signal::Short => {
                strategy.entry(DOWN, Short).size(1.0);
            }
            _ => {}
        }
        let (held, cost) = (strategy.position(), strategy.avg_cost());
        if held > 0.0 {
            strategy
                .exit("close long")
                .from(UP)
                .take(cost * (1.0 + TARGET))
                .stop(cost * (1.0 - STOP));
        }
        if held < 0.0 {
            strategy
                .exit("close short")
                .from(DOWN)
                .take(cost * (1.0 - TARGET))
                .stop(cost * (1.0 + STOP));
        }
    }
}
