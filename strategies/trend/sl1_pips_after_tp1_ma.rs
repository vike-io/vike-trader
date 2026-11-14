// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "SL1 Pips after TP1 (MA)" by ahmad_naquib, https://www.tradingview.com/script/tGTV8MkY/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2020-07-05, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! When flat, long 1,000 when EMA(10) crosses up through WMA(20) and short 1,000 when it crosses down. From the signal bar's close
//! (the entry level) and a step of 2.00: half the position leaves at a stop one step against or a target one step for it, the
//! other half at a stop starting at the same level and a target two steps for it. Once a bar trades through the first target
//! from an open short of it, the second stop moves to the entry level, or to a trailing level nearer the entry when that is
//! one. The exits are issued on every bar. No costs; starts with 100, and nothing refuses a position far larger than that.
//!
//! The trailing level is the first target or the open, whichever is further for the position, brought back one step, and it
//! ratchets while the previous bar opened beyond it.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Ema, Wma};
use vike_indicators::strategy_kit::{max, min};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const UP: &str = "buy";
const DOWN: &str = "sell";
const SIZE: f64 = 1_000.0;
const HALF: f64 = 500.0;
const STEP_TICKS: f64 = 200.0;

struct S {
    ema: Ema,
    wma: Wma,
    previous: (f64, f64),
    entry: f64,
    stop1: f64,
    stop2: f64,
    target1: f64,
    target2: f64,
    trail_long: f64,
    trail_short: f64,
    previous_open: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        ema: Ema::new(10.0).expect("a valid length"),
        wma: Wma::new(20.0).expect("a valid length"),
        previous: (f64::NAN, f64::NAN),
        entry: f64::NAN,
        stop1: f64::NAN,
        stop2: f64::NAN,
        target1: f64::NAN,
        target2: f64::NAN,
        trail_long: f64::NAN,
        trail_short: f64::NAN,
        previous_open: f64::NAN,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let step = STEP_TICKS * tick;
        let ema = self.ema.next(bar);
        let wma = self.wma.next(bar);
        let (previous_ema, previous_wma) = self.previous;
        self.previous = (ema, wma);
        let held = strategy.position();
        let buy = ema > wma && previous_ema <= previous_wma && held == 0.0;
        let sell = ema < wma && previous_ema >= previous_wma && held == 0.0;
        if buy || sell {
            let dir = if sell { -1.0 } else { 1.0 };
            self.entry = bar.close;
            self.stop1 = self.entry - step * dir;
            self.stop2 = self.stop1;
            self.target1 = self.entry + step * dir;
            self.target2 = self.entry + step * 2.0 * dir;
        }
        let long_side = buy || held > 0.0;
        let short_side = sell || held < 0.0;

        let trail_long = max(self.target1, bar.open) - step;
        let trail_short = min(self.target1, bar.open) + step;
        self.trail_long = if self.previous_open > self.trail_long {
            max(trail_long, self.trail_long)
        } else {
            trail_long
        };
        self.trail_short = if self.previous_open < self.trail_short {
            min(trail_short, self.trail_short)
        } else {
            trail_short
        };
        self.previous_open = bar.open;
        if long_side && bar.high > self.target1 && bar.open <= self.target1 {
            self.stop2 = min(self.entry, self.trail_long);
        }
        if short_side && bar.low < self.target1 && bar.open >= self.target1 {
            self.stop2 = max(self.entry, self.trail_short);
        }

        if buy && !short_side {
            strategy.entry(UP, Long).size(SIZE);
        }
        let first = (self.target1, self.stop1);
        let second = (self.target2, self.stop2);
        strategy.exit("exit1").from(UP).size(HALF).take(first.0).stop(first.1);
        strategy.exit("exit2").from(UP).size(HALF).take(second.0).stop(second.1);
        if sell && !long_side {
            strategy.entry(DOWN, Short).size(SIZE);
        }
        strategy.exit("exit3").from(DOWN).size(HALF).take(first.0).stop(first.1);
        strategy.exit("exit4").from(DOWN).size(HALF).take(second.0).stop(second.1);
    }
}
