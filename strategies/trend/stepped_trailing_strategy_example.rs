// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Stepped trailing strategy example" by adolgov, https://www.tradingview.com/script/jjhUHcje/
// The original's notice: © adolgov
// Changed: rewritten by hand in Rust for vike.

//! Long 1 when SMA(14) of the close crosses up through SMA(28). The exit steps with the trade, judged at each close: on the
//! entry bar a stop 5% under the entry and a target 15% over it; once a bar's high has been 5% over the entry the stop moves to
//! the entry, and once one has been 10% over it the stop moves to 5% over the entry, the target staying. The stage moves at
//! most one step a bar, and the exit rests from the bar after it is set. No costs; starts with 100,000.
//!
//! The rule writes each level as a number of ticks from the entry, `entry * percent / 100 / tick`, and takes it back the same
//! way, so a level is `entry -+ that many ticks * tick`.
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
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "My Long Entry Id";

struct S {
    fast: IndicatorLine,
    slow: IndicatorLine,
    cross: Cross,
    stage: u8,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: IndicatorLine::new("sma", &[14.0]),
        slow: IndicatorLine::new("sma", &[28.0]),
        cross: Cross::new(),
        stage: 0,
    })
}

/// `percent` of the entry, in ticks of `tick`.
fn points(entry: f64, percent: f64, tick: f64) -> f64 {
    entry * percent / 100.0 / tick
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        if matches!(self.cross.step(fast, slow), Signal::Long) {
            strategy.entry(NAME, Long).size(1.0);
        }

        let held = strategy.position();
        let entry = strategy.avg_cost();
        let gained = if held > 0.0 { (bar.high - entry) / tick } else { 0.0 };
        if held == 0.0 {
            self.stage = 0;
        }
        if self.stage == 0 && held != 0.0 {
            self.stage = 1;
        } else if self.stage == 1 && gained >= points(entry, 5.0, tick) {
            self.stage = 2;
        } else if self.stage == 2 && gained >= points(entry, 10.0, tick) {
            self.stage = 3;
        }
        let take = entry + points(entry, 15.0, tick) * tick;
        let cut = match self.stage {
            1 => entry - points(entry, 5.0, tick) * tick,
            2 => entry,
            3 => entry + points(entry, 5.0, tick) * tick,
            _ => {
                strategy.cancel("x");
                return;
            }
        };
        strategy.exit("x").from(NAME).take(take).stop(cut);
    }
}
