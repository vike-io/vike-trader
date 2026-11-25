// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "KALKI TFXBOT" by KalkiTrader007, https://www.tradingview.com/script/gFMT7BUb/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2026-04-28, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! A sweep of the last swing low or high, orders filling at the close. Swings are pivots of 10 bars before and 3 after. Long 1
//! when the low dips under the last swing low and the bar closes up, above it and above EMA(200), unless already long; short 1
//! on the mirror under the last swing high, unless already short. Each closes the opposite position first. A long's stop is
//! 0.05 under the bar's low and its target three times that risk above the close, a short's the mirror, both issued with the
//! entry. No costs; starts with 100.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! fill = "close"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{ConfirmedPivot, Ema};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const UP: &str = "Long";
const DOWN: &str = "Short";
const REWARD: f64 = 3.0;
/// Half a pip read as a tenth of the rule's 0.5.
const BUFFER: f64 = 0.05;

struct S {
    trend: Ema,
    pivots: ConfirmedPivot,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        trend: Ema::new(200.0).expect("a valid length"),
        pivots: ConfirmedPivot::new(10.0, 3.0).expect("valid lengths"),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let trend = self.trend.next(bar);
        let [_, _, res, sup] = self.pivots.next(bar);
        let held = strategy.position();
        let buy = bar.low < sup && bar.close > sup && bar.close > bar.open && bar.close > trend;
        let sell = bar.high > res && bar.close < res && bar.close < bar.open && bar.close < trend;
        if buy && held <= 0.0 {
            strategy.close(DOWN);
            let cut = bar.low - BUFFER;
            let take = bar.close + (bar.close - cut) * REWARD;
            strategy.entry(UP, Long).size(1.0);
            strategy.exit("Exit Long").from(UP).take(take).stop(cut);
        }
        if sell && held >= 0.0 {
            strategy.close(UP);
            let cut = bar.high + BUFFER;
            let take = bar.close - (cut - bar.close) * REWARD;
            strategy.entry(DOWN, Short).size(1.0);
            strategy.exit("Exit Short").from(DOWN).take(take).stop(cut);
        }
    }
}
