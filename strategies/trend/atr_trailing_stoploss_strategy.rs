// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "ATR Trailing Stoploss Strategy" by ceyhun, https://www.tradingview.com/script/NsD6sRKu/
// The original's notice: © ceyhun
// Changed: rewritten by hand in Rust for vike.

//! A trailing level: the close on the first 15 bars, then the highest of `high - 2.5 * ATR(5)` over the last 10 bars. Long 1
//! when the close crosses up through it, short 1 when it crosses down, each reversing the other. No costs; starts with 100,000.
//!
//! The rule writes the level as a choice between that highest value and a copy of it, so both arms are the same number.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Donchian;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const FIRST_BARS: usize = 15;

struct S {
    atr: IndicatorLine,
    highest: Donchian,
    seen: usize,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        atr: IndicatorLine::new("atr", &[5.0]),
        highest: Donchian::new(10.0).expect("a valid length"),
        seen: 0,
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let floor = bar.high - 2.5 * self.atr.next(bar)[0];
        let [highest, _, _] = self.highest.next_hl(floor, floor);
        self.seen += 1;
        let level = if self.seen <= FIRST_BARS { bar.close } else { highest };
        match self.cross.step(bar.close, level) {
            Signal::Long => {
                strategy.entry("long", Long).size(1.0);
            }
            Signal::Short => {
                strategy.entry("short", Short).size(1.0);
            }
            _ => {}
        }
    }
}
