// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Bollinger Stop Strategy" by ROBO_Trading, https://www.tradingview.com/script/jCFatPh6/
// The original's notice: © ROBO_Trading
// Changed: rewritten by hand in Rust for vike.

//! On every bar, a long stop at the upper Bollinger band (20-bar SMA of the close plus 2 population standard deviations) and
//! a short stop at the lower one, each all of the account's worth and each reversing the other. 0.1% commission; starts
//! with 10,000.
//!
//! A stop's size spends the account's worth at its level on the grid, or at this close when the close is already through it.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Bollinger;
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

struct S {
    bands: Bollinger,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S { bands: Bollinger::new(20.0, 2.0).expect("valid parameters") })
}

/// The price a stop entry at `level` is sized from.
fn sized_at(strategy: &Tactics<'_>, side: OrderSide, level: f64, close: f64) -> f64 {
    let level = strategy.snap_stop(side, level);
    if (side == Long && level <= close) || (side == Short && level >= close) {
        close
    } else {
        level
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.1), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let [upper, basis, lower] = self.bands.next(bar);
        if basis > 0.0 {
            let long = share_of_worth(&sizing, 100.0, sized_at(strategy, Long, upper, bar.close));
            strategy.entry("Long", Long).size(long).stop(upper);
            let short = share_of_worth(&sizing, 100.0, sized_at(strategy, Short, lower, bar.close));
            strategy.entry("Short", Short).size(short).stop(lower);
        }
    }
}
