// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Ema Scalp" by Shauryam_or, https://www.tradingview.com/script/kUncyS8L/
// The original's notice: © Shauryam_or
// Changed: rewritten by hand in Rust for vike.

//! Two counts of closes against EMA(100): bars closing above it since the last bar held short, and bars closing below it since
//! the last bar held long. When nothing is open, long 1 on the first such close above, short 1 on the first such close below.
//! When a trade closes in profit, the same side again at the next open, under its own name. Each entry, from the bar after it
//! filled, carries a stop 0.8% and a target 1% from the average cost, set again at every close. No costs; starts with
//! 1,000,000.
//!
//! "In profit" is the rule's latch: set on a bar where the count of winning trades grew, cleared on one where the count of
//! losing trades grew, the second read after the first.
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
use vike_tactics::{Rule, RuleDefaults, Tactics};

const FIRST_LONG: &str = "enter long";
const FIRST_SHORT: &str = "enter short";
const AGAIN_LONG: &str = "El2";
const AGAIN_SHORT: &str = "Es2";
const STOP: f64 = 0.8 / 100.0;
const TAKE: f64 = 1.0 / 100.0;

struct S {
    ema: IndicatorLine,
    above: u64,
    below: u64,
    held_before: f64,
    wins: usize,
    losses: usize,
    in_profit: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        ema: IndicatorLine::new("ema", &[100.0]),
        above: 0,
        below: 0,
        held_before: 0.0,
        wins: 0,
        losses: 0,
        in_profit: false,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let ema = self.ema.next(bar)[0];
        let long = bar.close > ema;
        let short = bar.close < ema;
        let held = strategy.position();
        if long {
            self.above += 1;
        }
        if held < 0.0 {
            self.above = 0;
        }
        if short {
            self.below += 1;
        }
        if held > 0.0 {
            self.below = 0;
        }

        let flat = strategy.lots().is_empty();
        if long && flat && self.above == 1 {
            strategy.entry(FIRST_LONG, Long).size(1.0);
        }
        if short && flat && self.below == 1 {
            strategy.entry(FIRST_SHORT, Short).size(1.0);
        }

        let before = self.held_before;
        self.held_before = held;
        let closed = (before != 0.0 && held == 0.0) || before * held < 0.0;
        let done = strategy.closed_trades().unwrap_or(&[]);
        let wins = done.iter().filter(|r| r.net > 0.0).count();
        let losses = done.iter().filter(|r| r.net < 0.0).count();
        if wins > self.wins {
            self.in_profit = true;
        }
        if losses > self.losses {
            self.in_profit = false;
        }
        (self.wins, self.losses) = (wins, losses);
        if self.in_profit && closed && before > 0.0 {
            strategy.entry(AGAIN_LONG, Long).size(1.0);
        }
        if self.in_profit && closed && before < 0.0 {
            strategy.entry(AGAIN_SHORT, Short).size(1.0);
        }

        let cost = strategy.avg_cost();
        if held > 0.0 {
            let (cut, take) = (cost * (1.0 - STOP), cost * (1.0 + TAKE));
            strategy.exit("Close Long").from(FIRST_LONG).take(take).stop(cut);
            strategy.exit("Close Long2").from(AGAIN_LONG).take(take).stop(cut);
        }
        if held < 0.0 {
            let (cut, take) = (cost * (1.0 + STOP), cost * (1.0 - TAKE));
            strategy.exit("Close Short").from(FIRST_SHORT).take(take).stop(cut);
            strategy.exit("Close Short2").from(AGAIN_SHORT).take(take).stop(cut);
        }
    }
}
