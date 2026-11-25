// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "MACD ReLoaded" by KivancOzbilgic, https://www.tradingview.com/script/VO6DY8s1/
// The original's notice: © KivancOzbilgic
// Changed: rewritten by hand in Rust for vike.

//! A MACD built from variable index averages of the close: the 12-bar one less the 26-bar one, less a 9-bar variable index
//! average of that difference. Long 1 when it crosses up through 0, short 1 when it crosses down, each reversing the other.
//! No costs; starts with 1,000,000.
//!
//! Each average starts at 0 and moves toward its input by `2 / (length + 1)` times the absolute 9-bar Chande momentum, read 0
//! until it exists: vike's `Vidya::with_zero_seed`, as the rule writes it.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Vidya;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

struct S {
    short: Vidya,
    long: Vidya,
    trigger: Vidya,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let var = |n: f64| Vidya::new(n, 9.0).expect("valid lengths").with_zero_seed(true);
    Box::new(S { short: var(12.0), long: var(26.0), trigger: var(9.0), cross: Cross::new() })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let macd = self.short.next_value(bar.close) - self.long.next_value(bar.close);
        let hist = macd - self.trigger.next_value(macd);
        match self.cross.step(hist, 0.0) {
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
