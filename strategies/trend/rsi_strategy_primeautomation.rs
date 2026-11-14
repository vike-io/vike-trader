// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "RSI Strategy [PrimeAutomation]" by ChartPrime, https://www.tradingview.com/script/RiQzLLoh/
// The original's notice: © PrimeAutomation
// Changed: rewritten by hand in Rust for vike.

//! An RSI(40) breakout with a trailing stop and a target. After 201 bars, a cross of the RSI up through 60 goes long and a cross
//! down through 35 short, when no trade is on (a flag set by an entry and cleared on the first bar the account is flat), each
//! 10% of the account's worth at the signal's close. The stop starts 4 ATR(200) from the close and the target 7 away; the stop
//! then trails to the bar's extreme less 4 ATR, a rising stop for a long, a falling one for a short, updated after the bar's
//! orders. The exit orders rest with the stop as it stood before that update, and a bar whose low under (high over) the stop as
//! the bar before left it also closes the position at the bar's close (`immediately`).
//!
//! `process_orders_on_close`: the entry and that close fill at the close that placed them, and the stop and target are the
//! intrabar orders. The 5-minute calls feed only an off switch (`MarketClosed`). Margins 0: no margin call. 0.06% commission;
//! starts with 10,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Atr, Rsi};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const UPPER: f64 = 60.0;
const LOWER: f64 = 35.0;
const STOP_ATRS: f64 = 4.0;
const TARGET_ATRS: f64 = 7.0;
/// `bar_index > 200`: the bars before the 202nd (the window's first is bar 0) place nothing.
const FIRST_BAR: usize = 202;

struct S {
    rsi: Rsi,
    atr: Atr,
    up: Cross,
    down: Cross,
    bars: usize,
    stop: f64,
    target: f64,
    long: Option<bool>,
    in_trade: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        rsi: Rsi::new(40.0).expect("a valid length").with_rs_form(true),
        atr: Atr::new(200.0).expect("a valid length"),
        up: Cross::new(),
        down: Cross::new(),
        bars: 0,
        stop: f64::NAN,
        target: f64::NAN,
        long: None,
        in_trade: false,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.06), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        self.bars += 1;
        let rsi = self.rsi.next_value(bar.close);
        let atr = self.atr.next(bar);
        let buy = matches!(self.up.step(rsi, UPPER), Signal::Long);
        let sell = matches!(self.down.step(rsi, LOWER), Signal::Short);
        // Read before this bar's orders change the stop.
        let close_long = bar.low < self.stop && self.long == Some(true);
        let close_short = bar.high > self.stop && self.long == Some(false);
        if self.bars < FIRST_BAR {
            return;
        }
        if strategy.position() == 0.0 {
            (self.stop, self.target, self.in_trade) = (f64::NAN, f64::NAN, false);
        }
        if buy && !self.in_trade {
            self.stop = bar.close - atr * STOP_ATRS;
            self.target = bar.close + atr * TARGET_ATRS;
            (self.long, self.in_trade) = (Some(true), true);
            strategy.close("Short");
            strategy.entry("Long", Long).size(share_of_worth(&sizing, 10.0, bar.close));
        }
        if sell && !self.in_trade {
            self.stop = bar.close + atr * STOP_ATRS;
            self.target = bar.close - atr * TARGET_ATRS;
            (self.long, self.in_trade) = (Some(false), true);
            strategy.close("Long");
            strategy.entry("Short", Short).size(share_of_worth(&sizing, 10.0, bar.close));
        }
        if !self.stop.is_nan() {
            let (take, cut) = (self.target, self.stop);
            strategy.exit("Long Exit").from("Long").take(take).stop(cut);
            strategy.exit("Short Exit").from("Short").take(take).stop(cut);
        }
        if close_long {
            strategy.close("Long").here();
        }
        if close_short {
            strategy.close("Short").here();
        }
        if self.in_trade {
            self.stop = if self.long == Some(true) {
                vike_indicators::strategy_kit::max(self.stop, bar.low - atr * STOP_ATRS)
            } else {
                vike_indicators::strategy_kit::min(self.stop, bar.high + atr * STOP_ATRS)
            };
        }
    }
}
