// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "5 ema strategy" by samscripter, https://www.tradingview.com/script/EaZHR7q6/
// The original's notice: © samscripter
// Changed: rewritten by hand in Rust for vike.

//! A bar wholly above EMA(5) of the close remembers its low, one wholly below remembers its high. While flat and under four
//! entries since the day began, long 1 when the high passes the high remembered on the last bar, with its stop at the lower
//! of this low and the last; short 1 when the low passes the low remembered, its stop at the higher of the two highs. A
//! pass forgets that level whether or not it entered. From the bar after the fill, while held, the stop stays and the target
//! lies 1.5 times the stop's distance from the average entry on the other side. Every order is judged at the bar's close;
//! 0.2 a contract on each fill; starts with 1,000,000.
//!
//! The count resets after the rule has run on the first bar of a UTC day, so that bar's entries still count against the
//! day before. The stop a trade uses is the one remembered when its position first showed, a bar after its entry.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! fill = "close"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_indicators::strategy_kit::{max, min};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::MS_PER_DAY;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::helpers::known;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const MAX_ENTRIES: u32 = 4;
const TAKE_PER_STOP: f64 = 1.5;

struct S {
    ema: IndicatorLine,
    hi: f64,
    lo: f64,
    count: u32,
    buy_stop_at_entry: f64,
    sell_stop_at_entry: f64,
    buy_stop: f64,
    sell_stop: f64,
    was_held: bool,
    last_high: f64,
    last_low: f64,
    day: Option<i64>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        ema: IndicatorLine::new("ema", &[5.0]),
        hi: f64::NAN,
        lo: f64::NAN,
        count: 0,
        buy_stop_at_entry: f64::NAN,
        sell_stop_at_entry: f64::NAN,
        buy_stop: f64::NAN,
        sell_stop: f64::NAN,
        was_held: false,
        last_high: f64::NAN,
        last_low: f64::NAN,
        day: None,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::per_contract(0.2), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let ema = self.ema.next(bar)[0];
        let (last_hi, last_lo) = (self.hi, self.lo);
        if bar.close > ema && bar.open > ema && bar.low > ema && bar.high > ema {
            self.lo = bar.low;
        }
        if bar.close < ema && bar.open < ema && bar.low < ema && bar.high < ema {
            self.hi = bar.high;
        }
        let held = strategy.position();
        if bar.high > last_hi {
            if held == 0.0 && self.count < MAX_ENTRIES {
                strategy.entry("El", Long).size(1.0);
                self.count += 1;
                self.buy_stop_at_entry = min(bar.low, self.last_low);
            }
            self.hi = f64::NAN;
        }
        if bar.low < last_lo {
            if held == 0.0 && self.count < MAX_ENTRIES {
                strategy.entry("Es", Short).size(1.0);
                self.count += 1;
                self.sell_stop_at_entry = max(bar.high, self.last_high);
            }
            self.lo = f64::NAN;
        }
        if held != 0.0 && !self.was_held {
            self.buy_stop = self.buy_stop_at_entry;
            self.sell_stop = self.sell_stop_at_entry;
        }
        self.was_held = held != 0.0;
        let avg = strategy.avg_cost();
        if held > 0.0 {
            let take = avg - (self.buy_stop - avg) * TAKE_PER_STOP;
            let exit = strategy.exit("XL").from("El");
            if let Some(take) = known(take) {
                exit.take(take);
            }
            if let Some(cut) = known(self.buy_stop) {
                exit.stop(cut);
            }
        }
        if held < 0.0 {
            let take = avg - (self.sell_stop - avg) * TAKE_PER_STOP;
            let exit = strategy.exit("XS").from("Es");
            if let Some(take) = known(take) {
                exit.take(take);
            }
            if let Some(cut) = known(self.sell_stop) {
                exit.stop(cut);
            }
        }
        let day = bar.timestamp.div_euclid(MS_PER_DAY);
        if self.day.is_some_and(|d| d != day) {
            self.count = 0;
        }
        self.day = Some(day);
        (self.last_high, self.last_low) = (bar.high, bar.low);
    }
}
