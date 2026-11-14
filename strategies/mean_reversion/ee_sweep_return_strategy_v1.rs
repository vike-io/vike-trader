// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "EE Sweep Return — STRATEGY v1" by everevolving365, https://www.tradingview.com/script/7OLnQI4B/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2026-09-12, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! An opening range, the high and low of the bars opening 08:30 to 09:30 Chicago time, then a signal window from 09:30 to
//! 15:00. In the window a bar whose high goes over the range arms a short, one whose low goes under it arms a long, and the
//! last such break is remembered (both in one bar: the bar's own direction). A bar that closes back inside the range fires the
//! armed side, the last break deciding when both are armed, and disarms both. When flat, that side's 1 unit is entered at this
//! close with a target 50 and a stop 25 away from it. Anything held is closed at the close of the 15:00 bar.
//!
//! Every order fills at the close of the bar that placed it. 2 a unit per fill, 1 tick of slippage; starts with 50,000.
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
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const TARGET: f64 = 50.0;
const STOP: f64 = 25.0;
/// Chicago's standard offset from UTC, in hours.
const STANDARD: i64 = -6;

struct S {
    range_high: f64,
    range_low: f64,
    range_ok: bool,
    arm_up: bool,
    arm_down: bool,
    last_break: i8,
    in_range_was: bool,
    in_window_was: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        range_high: f64::NAN,
        range_low: f64::NAN,
        range_ok: false,
        arm_up: false,
        arm_down: false,
        last_break: 0,
        in_range_was: false,
        in_window_was: false,
    })
}

fn min_max(accumulator: f64, x: f64, high: bool) -> f64 {
    if accumulator.is_nan() {
        x
    } else if high {
        accumulator.max(x)
    } else {
        accumulator.min(x)
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::PerContract(2.0), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let minute = us_minute(bar.timestamp, STANDARD);
        let in_range = (8 * 60 + 30..9 * 60 + 30).contains(&minute);
        let in_window = (9 * 60 + 30..15 * 60).contains(&minute);
        let range_start = in_range && !self.in_range_was;
        let range_end = !in_range && self.in_range_was;
        let window_end = !in_window && self.in_window_was;
        (self.in_range_was, self.in_window_was) = (in_range, in_window);

        if range_start {
            (self.range_high, self.range_low, self.range_ok) = (f64::NAN, f64::NAN, false);
            (self.arm_up, self.arm_down, self.last_break) = (false, false, 0);
        }
        if in_range {
            self.range_high = min_max(self.range_high, bar.high, true);
            self.range_low = min_max(self.range_low, bar.low, false);
        }
        if range_end {
            self.range_ok = true;
        }
        let active =
            self.range_ok && in_window && !self.range_high.is_nan() && !self.range_low.is_nan();
        if active {
            let up = bar.high > self.range_high;
            let down = bar.low < self.range_low;
            self.arm_up |= up;
            self.arm_down |= down;
            if up && down {
                self.last_break = if bar.close >= bar.open { 1 } else { -1 };
            } else if up {
                self.last_break = 1;
            } else if down {
                self.last_break = -1;
            }
        }
        let inside = active && bar.close < self.range_high && bar.close > self.range_low;
        let raw_short = self.arm_up && inside;
        let raw_long = self.arm_down && inside;
        let short = raw_short && (!raw_long || self.last_break == 1);
        let long = raw_long && (!raw_short || self.last_break == -1);
        if short || long {
            (self.arm_up, self.arm_down, self.last_break) = (false, false, 0);
        }

        let flat = strategy.position() == 0.0;
        if short && flat {
            strategy.entry("SR Short", Short).size(1.0);
            strategy.exit("X S").from("SR Short").take(bar.close - TARGET).stop(bar.close + STOP);
        }
        if long && flat {
            strategy.entry("SR Long", Long).size(1.0);
            strategy.exit("X L").from("SR Long").take(bar.close + TARGET).stop(bar.close - STOP);
        }
        if window_end && strategy.position() != 0.0 {
            strategy.close_all();
        }
    }
}
