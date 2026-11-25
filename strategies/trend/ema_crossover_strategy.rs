// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "EMA Crossover Strategy" by gregoirejohnb, https://www.tradingview.com/script/sxJejjdy/
// The original's notice: © gregoirejohnb
// Changed: rewritten by hand in Rust for vike.

//! Long 10% of the account's worth when EMA(20) of the bar's midpoint crosses up through EMA(60) of it, short 10% when it
//! crosses down, each reversing the other. 0.1% commission; starts with 100,000 in a USD account.
//!
//! The rule's start year (2020) holds on every bar here.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Ema;
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const SHARE: f64 = 10.0;

struct S {
    fast: Ema,
    slow: Ema,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: Ema::new(20.0).expect("a valid length"),
        slow: Ema::new(60.0).expect("a valid length"),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.1), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let mid = (bar.high + bar.low) / 2.0;
        let fast = self.fast.next_value(mid);
        let slow = self.slow.next_value(mid);
        match self.cross.step(fast, slow) {
            Signal::Long => {
                strategy.entry("long", Long).size(share_of_worth(&sizing, SHARE, bar.close));
            }
            Signal::Short => {
                strategy.entry("short", Short).size(share_of_worth(&sizing, SHARE, bar.close));
            }
            _ => {}
        }
    }
}
