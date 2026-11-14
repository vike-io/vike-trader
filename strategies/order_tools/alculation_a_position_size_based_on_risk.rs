// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Сalculation a position size based on risk" by adolgov, https://www.tradingview.com/script/hoCPm5UY/
// The original's notice: © adolgov
// Changed: rewritten by hand in Rust for vike.

//! Entries on a clock, sized from risk: long on every 333rd bar and short on every 444th, counting the window's first bar as 0,
//! each for `2%` of the account's worth over a stop 10% away, `worth * 0.02 / (close * 0.1)`. An entry reverses the other side.
//! While held, a stop 10% of the average cost past the entry. No costs; starts with 100,000.
//!
//! On a bar that is both, the rule asks for the long and then the short.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const LONG_NAME: &str = "My Long Entry Id";
const SHORT_NAME: &str = "My Short Entry Id";
const STOP_PERCENT: f64 = 10.0;
const RISK_PERCENT: f64 = 2.0;

struct S {
    bar_index: u64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S { bar_index: 0 })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let sizing = strategy.sizing();
        let qty = sizing
            .floor(strategy.equity() * RISK_PERCENT / 100.0 / (bar.close * STOP_PERCENT / 100.0));
        if self.bar_index.is_multiple_of(333) {
            strategy.entry(LONG_NAME, Long).size(qty);
        }
        if self.bar_index.is_multiple_of(444) {
            strategy.entry(SHORT_NAME, Short).size(qty);
        }
        self.bar_index += 1;
        let held = strategy.position();
        if let Some(lot) = strategy.lots().first() {
            // The stop lies `loss` ticks past the entry, `loss` being 10% of the average cost in ticks.
            let loss = strategy.avg_cost() * STOP_PERCENT / 100.0 / tick;
            let (name, entry) = (lot.name.clone(), lot.price);
            let cut = if held > 0.0 { entry - loss * tick } else { entry + loss * tick };
            strategy.exit(&format!("x {name}")).from(&name).stop(cut);
        }
    }
}
