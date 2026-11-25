// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "RSI Strategy with alerts via TradingConnector to Forex" by Peter_O, https://www.tradingview.com/script/kQY2gaLx/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2021-09-10, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Long 100,000 BTC when RSI(14) crosses up through 30, short 100,000 when it crosses down through 70, each reversing the
//! other. No costs; starts with 100,000, so every position is far larger than the account, which nothing refuses.
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

const SIZE: f64 = 100_000.0;
const OVERSOLD: f64 = 30.0;
const OVERBOUGHT: f64 = 70.0;

struct S {
    rsi: IndicatorLine,
    up: Cross,
    down: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S { rsi: IndicatorLine::new("rsi", &[14.0]), up: Cross::new(), down: Cross::new() })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let rsi = self.rsi.next(bar)[0];
        if matches!(self.up.step(rsi, OVERSOLD), Signal::Long) {
            strategy.entry("long", Long).size(SIZE);
        }
        if matches!(self.down.step(rsi, OVERBOUGHT), Signal::Short) {
            strategy.entry("short", Short).size(SIZE);
        }
    }
}
