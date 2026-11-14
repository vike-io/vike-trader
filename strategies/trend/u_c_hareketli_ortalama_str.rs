// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Üç Hareketli Ortalama Str." by dadashkadir, https://www.tradingview.com/script/sPAROdDQ/
// The original's notice: © dadashkadir
// Changed: rewritten by hand in Rust for vike.

//! Long all of the account's worth when SMA(7) of the close crosses up through SMA(25), short all of it when it crosses down,
//! each reversing the other. 0.047% commission; starts with 10,000.
//!
//! The rule also runs again after each of its fills inside a bar, and there it reads that bar's final values (`after_fill`):
//! that looks ahead, since a fill at the open meets a cross only the bar's close makes, and the reversal it orders fills at
//! the next point of the walk.
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
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

struct S {
    fast: Sma,
    slow: Sma,
    cross: Cross,
    /// The bar the averages have seen: a fill inside a bar feeds them ahead of its close.
    fed: Option<i64>,
    signal: Signal,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: Sma::new(7.0).expect("a valid length"),
        slow: Sma::new(25.0).expect("a valid length"),
        cross: Cross::new(),
        fed: None,
        signal: Signal::Hold,
    })
}

impl S {
    fn rule(&mut self, strategy: &mut Tactics<'_>, bar: &Bar, price: f64) {
        let sizing = strategy.sizing();
        if self.fed != Some(bar.timestamp) {
            self.fed = Some(bar.timestamp);
            let (fast, slow) = (self.fast.next(bar), self.slow.next(bar));
            self.signal = self.cross.step(fast, slow);
        }
        match self.signal {
            Signal::Long => {
                strategy.entry("LONG", Long).size(share_of_worth(&sizing, 100.0, price));
            }
            Signal::Short => {
                strategy.entry("SHORT", Short).size(share_of_worth(&sizing, 100.0, price));
            }
            _ => {}
        }
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.047), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        self.rule(strategy, bar, bar.close);
    }

    fn after_fill(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        self.rule(strategy, bar, bar.open);
    }
}
