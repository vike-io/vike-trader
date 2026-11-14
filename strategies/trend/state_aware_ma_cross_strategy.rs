// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "State-aware MA Cross Strategy" by chikaharu, https://www.tradingview.com/script/FgwP9BhA/
// The original's notice: © chikaharu
// Changed: rewritten by hand in Rust for vike.

//! A cross of two averages of the close, the pair picked each bar by the state of EMA(20): whether it rose on this bar and
//! whether the close is above it. Falling and below: EMA(15) against HMA(24); falling and above: SMA(19) against RMA(45);
//! rising and below: RMA(16) against HMA(59); rising and above: RMA(12) against RMA(36). Long all of the account's worth when
//! the picked fast line crosses up through the picked slow one, the whole position closed when it crosses down; a cross
//! compares this bar's picked pair with the pair picked on the bar before. No costs; starts with 1,000,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Ema, Hma, Sma, Smma};
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

struct S {
    ema15: Ema,
    hma24: Hma,
    sma19: Sma,
    rma45: Smma,
    rma16: Smma,
    hma59: Hma,
    rma12: Smma,
    rma36: Smma,
    base: Ema,
    last_base: f64,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let rma = |n: f64| Smma::new(n).expect("a valid length");
    Box::new(S {
        ema15: Ema::new(15.0).expect("a valid length"),
        hma24: Hma::new(24.0).expect("a valid length"),
        sma19: Sma::new(19.0).expect("a valid length"),
        rma45: rma(45.0),
        rma16: rma(16.0),
        hma59: Hma::new(59.0).expect("a valid length"),
        rma12: rma(12.0),
        rma36: rma(36.0),
        base: Ema::new(20.0).expect("a valid length"),
        last_base: f64::NAN,
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let c = bar.close;
        let pairs = [
            (self.ema15.next(bar), self.hma24.next_value(c)),
            (self.sma19.next_value(c), self.rma45.next_value(c)),
            (self.rma16.next_value(c), self.hma59.next_value(c)),
            (self.rma12.next_value(c), self.rma36.next_value(c)),
        ];
        let base = self.base.next(bar);
        let rising = base - std::mem::replace(&mut self.last_base, base) > 0.0;
        let above = c > base;
        let (fast, slow) = pairs[usize::from(rising) * 2 + usize::from(above)];
        match self.cross.step(fast, slow) {
            Signal::Long => {
                strategy.entry("long", Long).size(share_of_worth(&sizing, 100.0, c));
            }
            Signal::Short => {
                strategy.close_all();
            }
            _ => {}
        }
    }
}
