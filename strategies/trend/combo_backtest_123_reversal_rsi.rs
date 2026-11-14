// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Combo Backtest 123 Reversal & RSI" by HPotter, https://www.tradingview.com/script/IuTkaU1w/
// The original's notice: Copyright by HPotter v1.0 16/08/2021
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2021-08-16, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Two votes, each held until it changes. The first: up after two rising closes with the 14-bar stochastic under its 3-bar
//! SMA and over 50, down after two falling closes with it over its SMA and under 50. The second: up while RSI(12) is over 70,
//! down while under 30. Long 1 while both say up, short 1 while both say down, each reversing the other; everything closed
//! while they differ. No costs; starts with 100,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Rsi, Stochastic};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const STOCH: f64 = 14.0;
const SMOOTH: f64 = 3.0;
const LEVEL: f64 = 50.0;
const OVERSOLD: f64 = 30.0;
const OVERBOUGHT: f64 = 70.0;

struct S {
    /// The raw %K and its SMA.
    stoch: Stochastic,
    rsi: Rsi,
    /// The last two closes, older first.
    closes: [f64; 2],
    reversal: i8,
    strength: i8,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        stoch: Stochastic::new(STOCH, 1.0, SMOOTH).expect("valid lengths").with_fresh_sum(true),
        rsi: Rsi::new(12.0).expect("a valid length"),
        closes: [f64::NAN; 2],
        reversal: 0,
        strength: 0,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let [fast, slow] = self.stoch.next(bar);
        let [older, last] = self.closes;
        let close = bar.close;
        if older < last && close > last && fast < slow && fast > LEVEL {
            self.reversal = 1;
        } else if older > last && close < last && fast > slow && fast < LEVEL {
            self.reversal = -1;
        }
        self.closes = [last, close];
        let rsi = self.rsi.next(bar);
        if rsi > OVERBOUGHT {
            self.strength = 1;
        } else if rsi < OVERSOLD {
            self.strength = -1;
        }
        match (self.reversal, self.strength) {
            (1, 1) => {
                strategy.entry("long", Long).size(1.0);
            }
            (-1, -1) => {
                strategy.entry("short", Short).size(1.0);
            }
            _ => {
                strategy.close_all();
            }
        }
    }
}
