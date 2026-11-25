// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Stop loss and Take Profit in $$ example" by adolgov, https://www.tradingview.com/script/IHVPG6TS/
// The original's notice: © adolgov
// Changed: rewritten by hand in Rust for vike.

//! Long 1 when SMA(14) of the close crosses up through SMA(28), short 1 when it crosses down, each reversing the other. While
//! a position is held, one exit protects it: a target 200 of money away from its entry price and a stop 100 away, each
//! divided by the size held (so 200 and 100 of price at size 1). No costs; starts with 100,000.
//!
//! The exit is issued on every bar, with levels only while something is held, so a new position is unprotected on the bar
//! its entry fills and protected from the next.
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
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const TAKE_MONEY: f64 = 200.0;
const CUT_MONEY: f64 = 100.0;

struct S {
    fast: IndicatorLine,
    slow: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: IndicatorLine::new("sma", &[14.0]),
        slow: IndicatorLine::new("sma", &[28.0]),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        match self.cross.step(fast, slow) {
            Signal::Long => {
                strategy.entry("long", Long).size(1.0);
            }
            Signal::Short => {
                strategy.entry("short", Short).size(1.0);
            }
            _ => {}
        }
        let held = strategy.position();
        if held == 0.0 {
            return;
        }
        // The rule's distances are in ticks: money / size / tick, then back to price at the tick.
        let ticks = |money: f64| money / held.abs() / tick;
        let side = held.signum();
        let entry = strategy.avg_cost();
        let lots = if held > 0.0 { "long" } else { "short" };
        strategy
            .exit("x")
            .from(lots)
            .take(entry + side * ticks(TAKE_MONEY) * tick)
            .stop(entry - side * ticks(CUT_MONEY) * tick);
    }
}
