// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "ATR Trailing Stop Strategy by ceyhun" by ceyhun, https://www.tradingview.com/script/cj3MJWNz/
// The original's notice: © ceyhun
// Changed: rewritten by hand in Rust for vike.

//! Two ATR trailing levels of the close: a fast one 0.5 ATR(5) away and a slow one 3 ATR(10) away. A level ratchets toward
//! the close while the close stays on one side of it two bars running, and jumps to the other side when the close crosses it.
//! Long 1 when the fast level crosses up through the slow one, short 1 when it crosses down, each reversing the other. No
//! costs; starts with 100,000.
//!
//! A level is undefined while its ATR warms up, and then starts from 0, as the rule's `nz` reads it, which makes its first
//! value `close - loss`: vike's `AtrTrailingStop`.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::AtrTrailingStop;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

struct S {
    fast: AtrTrailingStop,
    slow: AtrTrailingStop,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: AtrTrailingStop::new(5.0, 0.5).expect("valid parameters"),
        slow: AtrTrailingStop::new(10.0, 3.0).expect("valid parameters"),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let fast = self.fast.next(bar);
        let slow = self.slow.next(bar);
        match self.cross.step(fast, slow) {
            Signal::Long => {
                strategy.entry("buy", Long).size(1.0);
            }
            Signal::Short => {
                strategy.entry("sell", Short).size(1.0);
            }
            _ => {}
        }
    }
}
