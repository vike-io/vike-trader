// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "How To Set Time Ranges" by allanster, https://www.tradingview.com/script/xAEG4ZJG/
// The original's notice: © allanster
// Changed: rewritten by hand in Rust for vike.

//! Long 100,000 worth when SMA(14) crosses up through SMA(28), closed when it crosses down. 0.27% commission; starts with
//! 100,000, and keeps buying that much however much is left.
//!
//! The rule's date range (2021 to 2112) and its entry and exit sessions (any time, every day) hold on every bar here.
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
use vike_model::Commission;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::cash_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "long";
const CASH: f64 = 100_000.0;

struct S {
    fast: IndicatorLine,
    slow: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: IndicatorLine::new("sma", &[14.0]),
        slow: IndicatorLine::new("sma", &[28.0]),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.27), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        match self.cross.step(fast, slow) {
            Signal::Long => {
                strategy.entry(NAME, Long).size(cash_worth(&sizing, CASH, bar.close));
            }
            Signal::Short => {
                strategy.close(NAME);
            }
            _ => {}
        }
    }
}
