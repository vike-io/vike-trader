// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Triple CCI Strategy MFI Confirmed [Skyrexio]" by Skyrexio, https://www.tradingview.com/script/PETNavGq/
// The original's notice: © Skyrexio
// Changed: rewritten by hand in Rust for vike.

//! Long 50% of the account's worth when the fast CCI (14, of the bar's typical price) crosses up through 0 with the close above
//! EMA(50), the CCIs of 25 and 50 bars above 0 and the money flow index (14) above 50, only on the bars from 2023-01-01 to
//! 2025-01-01 and one trade at a time. It closes at that bar's close when the close is under the take-profit line or the low is
//! at or under the stop 1.75 ATR(14) below the entry price. The take-profit line is missing while no trade is open and until,
//! after the trade opened, the high has crossed over the activation level (2.25 ATR above the entry price) from under it or from
//! no level; then it is EMA(20) of the close. 0.1% commission, 5 ticks of slippage; starts with 10,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Atr, Cci, Ema, Mfi};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::calendar::timestamp;
use vike_tactics::helpers::{BarsSince, fewer};
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "entry1";

struct S {
    fast: Cci,
    middle: Cci,
    slow: Cci,
    mfi: Mfi,
    atr: Atr,
    trailing: Ema,
    trend: Ema,
    cross: Cross,
    opened: BarsSince,
    crossed: BarsSince,
    open_before: usize,
    high_before: f64,
    level_before: f64,
    from: i64,
    to: i64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let cci = |n: f64| Cci::new(n).expect("a valid length");
    Box::new(S {
        fast: cci(14.0),
        middle: cci(25.0),
        slow: cci(50.0),
        mfi: Mfi::new(14.0).expect("a valid length"),
        atr: Atr::new(14.0).expect("a valid length"),
        trailing: Ema::new(20.0).expect("a valid length"),
        trend: Ema::new(50.0).expect("a valid length"),
        cross: Cross::new(),
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
        let (h, l, c) = (bar.high, bar.low, bar.close);
        let fast = self.fast.next_hlc(h, l, c);
        let (middle, slow) = (self.middle.next_hlc(h, l, c), self.slow.next_hlc(h, l, c));
        let mfi = self.mfi.next(bar);
        let atr = self.atr.next(bar);
        let (trailing, trend) = (self.trailing.next(bar), self.trend.next(bar));
        let crossed_zero = matches!(self.cross.step(fast, 0.0), Signal::Long);
        let entry = strategy.avg_cost();
        let (stop, level) = (entry - 1.75 * atr, entry + 2.25 * atr);
        let open = strategy.lots().len();

        let since_open = self.opened.step(open == 1 && self.open_before == 0);
        let from_below = self.high_before < self.level_before || self.level_before.is_nan();
        let since_cross = self.crossed.step(from_below && h > level);
        (self.open_before, self.high_before, self.level_before) = (open, h, level);
        let line = if open == 0 || fewer(since_open, since_cross) || since_cross.is_none() {
            f64::NAN
        } else {
            trailing
        };

        let long = crossed_zero && c > trend && middle > 0.0 && slow > 0.0 && mfi > 50.0;
        let closing = c < line || l <= stop;
        if long && bar.timestamp >= self.from && bar.timestamp <= self.to {
            strategy.entry(NAME, Long).size(share_of_worth(&sizing, 50.0, sizing.slipped(Long, c)));
        }
        if closing {
            strategy.close(NAME).here();
        }
    }
}
