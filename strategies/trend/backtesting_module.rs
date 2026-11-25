// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Backtesting Module" by dg_factor, https://www.tradingview.com/script/uLY2TQdm/
// The original's notice: © dg_factor [26.11.2023, Istanbul]
// Changed: rewritten by hand in Rust for vike.

//! The module's default test: long 10% of the account's worth when SMA(50) of the close crosses up through SMA(200), short
//! 10% when it crosses down, each reversing the other. 0.04% commission; starts with 10,000.
//!
//! The rule's own conditions are off, so the default test runs. Its window runs from the first bar to the run's clock
//! (`timenow`), which holds every bar the run feeds it, so the window is the run's own; its win and loss tallies only fill a
//! table, and its `barstate` reads draw it.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Sma;
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const SHARE: f64 = 10.0;

struct S {
    fast: Sma,
    slow: Sma,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: Sma::new(50.0).expect("a valid length"),
        slow: Sma::new(200.0).expect("a valid length"),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.04), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let fast = self.fast.next(bar);
        let slow = self.slow.next(bar);
        match self.cross.step(fast, slow) {
            Signal::Long => {
                strategy.entry("Long", Long).size(share_of_worth(&sizing, SHARE, bar.close));
            }
            Signal::Short => {
                strategy.entry("Short", Short).size(share_of_worth(&sizing, SHARE, bar.close));
            }
            _ => {}
        }
    }
}
