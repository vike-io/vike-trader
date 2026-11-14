// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from the script by PineIndicators, https://www.tradingview.com/script/gMeYb8Dp/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2025-03-31, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! A triangular Hull average of the close: WMA(40) of `3 * WMA(13) - WMA(20) - WMA(40)`, the lengths 40 / 3 and 40 / 2 cut to
//! whole bars. Long 1% of the account's worth when it crosses up through its own value 2 bars back, short it when it crosses
//! down, each reversing the other. 0.01% commission and 2 ticks of slippage; starts with 1,000.
//!
//! The rule's size is `default_qty_value = 1` with no type, which the reference reads as a percent of the account's worth. Its
//! volatility line and dashboard trade nothing.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

use vike_indicators::indicators::Wma;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;

const LENGTH: f64 = 40.0;
const SHARE: f64 = 1.0;
const BACK: usize = 2;

struct S {
    third: Wma,
    half: Wma,
    whole: Wma,
    outer: Wma,
    /// The average, newest first, `BACK + 1` of them.
    seen: VecDeque<f64>,
    cross: Cross,
}

fn wma(n: f64) -> Wma {
    Wma::new(n).expect("a valid length")
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        third: wma((LENGTH / 3.0).floor()),
        half: wma((LENGTH / 2.0).floor()),
        whole: wma(LENGTH),
        outer: wma(LENGTH),
        seen: VecDeque::with_capacity(BACK + 2),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.01), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let c = bar.close;
        let inner =
            self.third.next_value(c) * 3.0 - self.half.next_value(c) - self.whole.next_value(c);
        // The outer average starts at the inner one's first number, so its warm-up NaNs never enter its window.
        let thma = if inner.is_nan() { f64::NAN } else { self.outer.next_value(inner) };
        self.seen.push_front(thma);
        self.seen.truncate(BACK + 1);
        let back = self.seen.get(BACK).copied().unwrap_or(f64::NAN);
        match self.cross.step(thma, back) {
            Signal::Long => {
                let size = share_of_worth(&sizing, SHARE, sizing.slipped(Long, c));
                strategy.entry("Long", Long).size(size);
            }
            Signal::Short => {
                let size = share_of_worth(&sizing, SHARE, sizing.slipped(Short, c));
                strategy.entry("Short", Short).size(size);
            }
            _ => {}
        }
    }
}
