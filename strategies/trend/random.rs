// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "random" by SoftKill21, https://www.tradingview.com/script/FEJvYRkw/
// The original's notice: © SoftKill21
// Changed: rewritten by hand in Rust for vike.

//! Long only, on Heikin-Ashi candles of the chart's own bars: long all of the account's worth when the candle's close is above
//! its open and above the last candle's high, which itself rose above the one before; closed when the candle's close is
//! below its open and below the last candle's low. Orders fill at the next open, at the real price. 0.1% commission; starts
//! with 1,000.
//!
//! The candles are the rule's Heikin-Ashi ticker (`vike_indicators::strategy_kit::HeikinAshiCandles`): every price on the symbol's tick, candles from
//! the rule's first bar.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::strategy_kit::HeikinAshiCandles;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const NAME: &str = "long";

struct S {
    candles: HeikinAshiCandles,
    /// The last two candles' highs, newest first, and the last one's low.
    highs: [f64; 2],
    low: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S { candles: HeikinAshiCandles::default(), highs: [f64::NAN; 2], low: f64::NAN })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.1), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let Bar { open, high, low, close, .. } = self.candles.next(bar, strategy.tick());
        let [high1, high2] = self.highs;
        if close > high1 && close > open && high1 > high2 {
            strategy.entry(NAME, Long).size(share_of_worth(&sizing, 100.0, bar.close));
        }
        if close < open && close < self.low {
            strategy.close(NAME);
        }
        self.highs = [high, high1];
        self.low = low;
    }
}
