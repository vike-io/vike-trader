// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from the script by PineIndicators, https://www.tradingview.com/script/v0XszvRG/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2025-03-13, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Long all of the account's worth when EMA(12) crosses up through EMA(25) while flat, closed when it crosses down; both fill at
//! the signal bar's close. 0.05% commission; starts with 100,000.
//!
//! The rule's seven filters are all off by default, and its other directions are inputs not taken. The levels it draws trade
//! nothing.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const NAME: &str = "Long";

struct S {
    fast: IndicatorLine,
    slow: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: IndicatorLine::new("ema", &[12.0]),
        slow: IndicatorLine::new("ema", &[25.0]),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        match self.cross.step(fast, slow) {
            Signal::Long if strategy.position() <= 0.0 => {
                strategy.entry(NAME, Long).size(share_of_worth(&sizing, 100.0, bar.close));
            }
            Signal::Short if strategy.position() > 0.0 => {
                strategy.close(NAME);
            }
            _ => {}
        }
    }
}
