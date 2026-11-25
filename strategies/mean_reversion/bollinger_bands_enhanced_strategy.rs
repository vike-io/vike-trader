// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Bollinger Bands Enhanced Strategy" by Skyrexio, https://www.tradingview.com/script/V4B5nKIx/
// The original's notice: © Skyrex
// Changed: rewritten by hand in Rust for vike.

//! Long 30% of the account's worth when the bar's low is above EMA(200) and at or under the lower Bollinger band (SMA(20) of the
//! close less 2 population standard deviations), only on the bars from 2023-01-01 to 2025-01-01 and one trade at a time. It
//! closes at that bar's close when the close is under the take-profit line or the low is at or under the stop 1.75 ATR(14)
//! below the entry price. The take-profit line is missing while no trade is open and while, since the trade opened, the high has
//! crossed over the activation level (2.25 ATR above the entry price) less recently than the trade opened; otherwise it is the
//! SMA(20). 0.1% commission, 5 ticks of slippage; starts with 10,000.
//!
//! The comparison of two bars-since counts is false while either is missing, so before the high has ever crossed the level the
//! line is the SMA from the first bar of the trade on.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Atr, Bollinger, Ema};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::calendar::timestamp;
use vike_tactics::helpers::{BarsSince, fewer};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "entry1";

struct S {
    bands: Bollinger,
    atr: Atr,
    trend: Ema,
    /// Bars since a trade opened, and since the high crossed over the activation level.
    opened: BarsSince,
    crossed: BarsSince,
    open_before: usize,
    high_before: f64,
    level_before: f64,
    from: i64,
    to: i64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        bands: Bollinger::new(20.0, 2.0).expect("valid parameters"),
        atr: Atr::new(14.0).expect("a valid length"),
        trend: Ema::new(200.0).expect("a valid length"),
        opened: BarsSince::default(),
        crossed: BarsSince::default(),
        open_before: 0,
        high_before: f64::NAN,
        level_before: f64::NAN,
        from: timestamp(2023, 1, 1, 0, 0),
        to: timestamp(2025, 1, 1, 0, 0),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.1), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let [_, basis, lower] = self.bands.next(bar);
        let atr = self.atr.next(bar);
        let trend = self.trend.next(bar);
        let entry = strategy.avg_cost();
        let (stop, level) = (entry - 1.75 * atr, entry + 2.25 * atr);
        let open = strategy.lots().len();

        let since_open = self.opened.step(open == 1 && self.open_before == 0);
        let crossed_over = bar.high > level && self.high_before <= self.level_before;
        let since_cross = self.crossed.step(crossed_over);
        (self.open_before, self.high_before, self.level_before) = (open, bar.high, level);
        let line = if open == 0 || fewer(since_open, since_cross) { f64::NAN } else { basis };

        let closing = bar.close < line || bar.low <= stop;
        if bar.low > trend
            && bar.low <= lower
            && bar.timestamp >= self.from
            && bar.timestamp <= self.to
        {
            strategy.entry(NAME, Long).size(share_of_worth(
                &sizing,
                30.0,
                sizing.slipped(Long, bar.close),
            ));
        }
        if closing {
            strategy.close(NAME).here();
        }
    }
}
