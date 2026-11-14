// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Captain Backtest Model [TFO]" by tradeforopp, https://www.tradingview.com/script/tOQ8gnxj/
// The original's notice: © tradeforopp
// Changed: rewritten by hand in Rust for vike.

//! Each New York day: the high and low of the bars opening 06:00-10:00 make a range; the first bar opening 10:00-11:15 whose
//! high takes the range high sets a long bias, or failing that whose low takes the range low a short one. From 10:00 to 16:00,
//! with a long bias: once a bar has closed down and one has taken the last bar's low, a close above the last bar's high (the
//! bias already set on the last bar) buys 1 at the next open; a short bias mirrors it. One trade a day. The fill bar's open is
//! the entry: from that bar's close a stop 25 below and a target 75 above it, mirrored for a short, issued on every bar held.
//! The first bar after 16:00 clears the day and closes what is held at the next open. No costs; starts with 1,000,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::helpers::us_minute;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const UP: &str = "long";
const DOWN: &str = "short";
const RISK: f64 = 25.0;
const REWARD: f64 = 75.0;
/// New York's standard offset from UTC, in hours.
const NEW_YORK: i64 = -5;

struct S {
    range_high: f64,
    range_low: f64,
    bias: Option<bool>,
    long: bool,
    short: bool,
    opposite_close: bool,
    took_last: bool,
    entry: f64,
    /// `(stop, target)` of the trade.
    levels: (f64, f64),
    last_high: f64,
    last_low: f64,
    traded_last: bool,
    held_last: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        range_high: f64::NAN,
        range_low: f64::NAN,
        bias: None,
        long: false,
        short: false,
        opposite_close: false,
        took_last: false,
        entry: f64::NAN,
        levels: (f64::NAN, f64::NAN),
        last_high: f64::NAN,
        last_low: f64::NAN,
        traded_last: false,
        held_last: 0.0,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let minute = us_minute(bar.timestamp, NEW_YORK);
        let in_range = (360..600).contains(&minute);
        let deciding = (600..675).contains(&minute);
        let trading = (600..960).contains(&minute);
        let bias_before = self.bias;
        if in_range {
            self.range_high =
                if self.range_high.is_nan() { bar.high } else { self.range_high.max(bar.high) };
            self.range_low =
                if self.range_low.is_nan() { bar.low } else { self.range_low.min(bar.low) };
        }
        let day_over = !trading && self.traded_last;
        if day_over {
            self.range_high = f64::NAN;
            self.range_low = f64::NAN;
            self.bias = None;
            self.entry = f64::NAN;
            self.long = false;
            self.short = false;
            self.opposite_close = false;
            self.took_last = false;
        }
        if deciding && self.bias.is_none() {
            if bar.high > self.range_high {
                self.bias = Some(true);
            } else if bar.low < self.range_low {
                self.bias = Some(false);
            }
        }
        let held = strategy.position();
        if trading {
            match self.bias {
                Some(true) => {
                    self.opposite_close |= bar.close < bar.open;
                    self.took_last |= bar.low < self.last_low;
                }
                Some(false) => {
                    self.opposite_close |= bar.close > bar.open;
                    self.took_last |= bar.high > self.last_high;
                }
                None => {}
            }
            let ready = self.opposite_close && self.took_last;
            if bias_before == Some(true) && bar.close > self.last_high && ready && !self.long {
                self.long = true;
            }
            if bias_before == Some(false) && bar.close < self.last_low && ready && !self.short {
                self.short = true;
            }
            if self.long && self.entry.is_nan() {
                strategy.entry(UP, Long).size(1.0);
            } else if self.short && self.entry.is_nan() {
                strategy.entry(DOWN, Short).size(1.0);
            }
            if self.held_last == 0.0 && held != 0.0 {
                self.entry = bar.open;
                let side = if held > 0.0 { 1.0 } else { -1.0 };
                self.levels = (self.entry - side * RISK, self.entry + side * REWARD);
            }
            let (stop, target) = self.levels;
            if held > 0.0 {
                strategy.exit("long exit").from(UP).take(target).stop(stop);
            }
            if held < 0.0 {
                strategy.exit("short exit").from(DOWN).take(target).stop(stop);
            }
        }
        if day_over {
            strategy.close_all();
        }
        self.traded_last = trading;
        self.held_last = held;
        self.last_high = bar.high;
        self.last_low = bar.low;
    }
}
