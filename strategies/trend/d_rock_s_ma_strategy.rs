// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "D_Rock's MA Strategy" by D_Rockefeller, https://www.tradingview.com/script/uNIA4siU/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2024-07-14, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Long only. Enter when EMA(5) crosses up through EMA(10) on the same bar the MACD (12, 26, 9) line crosses up through its
//! signal, with the close over EMA(50) and no trade open. The size is the smaller of 1% of the account's worth risked on a
//! stop 1.4 ATR(14) under, and 25% of its worth at the close. Once held, while neither exit below fires: a stop 1.4 and a
//! target 3.2 times the ATR of the signal bar, from the average cost. EMA(5) crossing back down through EMA(10) closes the
//! position at the next open, and so does the eighth bar after the signal. 0.10 a fill; starts with 25,000, kept in USD.
//!
//! The trade-history read in the rule (the entry bar of the open trade) only places a label.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! stacked_closes = true
//! ```

use vike_indicators::indicators::{Atr, Ema, Macd};
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const NAME: &str = "LONG";
const MAX_BARS: usize = 8;

struct S {
    fast: Ema,
    medium: Ema,
    slow: Ema,
    macd: Macd,
    atr: Atr,
    ma_cross: Cross,
    macd_cross: Cross,
    /// The ATR at the last entry signal, and the bars since it.
    entry_atr: f64,
    since: Option<usize>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let ema = |n: f64| Ema::new(n).expect("a valid length");
    Box::new(S {
        fast: ema(5.0),
        medium: ema(10.0),
        slow: ema(50.0),
        macd: Macd::new(12.0, 26.0, 9.0).expect("valid lengths"),
        atr: Atr::new(14.0).expect("a valid length"),
        ma_cross: Cross::new(),
        macd_cross: Cross::new(),
        entry_atr: f64::NAN,
        since: None,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::PerOrder(0.10), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let close = bar.close;
        let (fast, medium, slow) =
            (self.fast.next(bar), self.medium.next(bar), self.slow.next(bar));
        let [line, signal, _] = self.macd.next(bar);
        let atr = self.atr.next(bar);
        let ma_cross = self.ma_cross.step(fast, medium);
        let macd_up = matches!(self.macd_cross.step(line, signal), Signal::Long);
        let cross_up = matches!(ma_cross, Signal::Long);
        let cross_exit = matches!(ma_cross, Signal::Short);

        let flat = strategy.position() == 0.0;
        let enter = cross_up && close > slow && macd_up && flat;
        self.since = if enter { Some(0) } else { self.since.map(|n| n + 1) };
        if enter {
            self.entry_atr = atr;
            let worth = sizing.equity();
            let risk = worth * 0.01 / (atr * 1.4);
            let equity_based = worth * 0.25 / close;
            strategy.entry(NAME, Long).size(sizing.floor(risk.min(equity_based)));
        }

        if strategy.position() > 0.0 {
            if cross_exit || self.since.is_some_and(|n| n >= MAX_BARS) {
                strategy.close_all();
            } else {
                let cost = strategy.avg_cost();
                strategy
                    .exit("Long SL/TP Exit")
                    .from(NAME)
                    .take(cost + self.entry_atr * 3.2)
                    .stop(cost - self.entry_atr * 1.4);
            }
        }
    }
}
