// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "RSI Crossover Strategy" by reees, https://www.tradingview.com/script/ZINSYeR8/
// The original's notice: © reees
// Changed: rewritten by hand in Rust for vike.

//! Long all of the account's worth when RSI(50) of the close crosses up through its 25-bar SMA; closed when it crosses down.
//! The rule asks to run on every tick, which on a closed bar is the bar's close. No costs; starts with 1,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Rsi;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::rolling::RollingSum;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "Long";
const AVERAGE: usize = 25;

struct S {
    rsi: Rsi,
    sum: RollingSum,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        rsi: Rsi::new(50.0).expect("a valid length"),
        sum: RollingSum::new(AVERAGE),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let rsi = self.rsi.next(bar);
        let average = self.sum.next(rsi) / AVERAGE as f64;
        match self.cross.step(rsi, average) {
            Signal::Long => {
                strategy.entry(NAME, Long).size(share_of_worth(&sizing, 100.0, bar.close));
            }
            Signal::Short => {
                strategy.close(NAME);
            }
            _ => {}
        }
    }
}
