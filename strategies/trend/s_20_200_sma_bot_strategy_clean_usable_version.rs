// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "20 / 200 SMA Bot Strategy - Clean Usable Version" by gpmpastorsc, https://www.tradingview.com/script/ZqnuKIAw/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2026-05-17, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! When flat and SMA(20) is above SMA(200), long 1 on the close crossing up through SMA(20); when flat and SMA(20) is under it,
//! short 1 on the close crossing down through it. Orders fill at the close. Each entry carries a target 100 past the close and
//! a stop at the last opposing bar beyond SMA(200), less 1: for a long the low of the last bar that closed down and under
//! SMA(200), within 300 bars, and for a short the high of the last that closed up and over it. A stop not on the far side of
//! the close, or with no such bar, is 30 from the close instead. No costs; starts with 50,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Sma;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const TARGET: f64 = 100.0;
const FALLBACK: f64 = 30.0;
const BUFFER: f64 = 1.0;
const LOOKBACK: i64 = 300;

struct S {
    fast: Sma,
    slow: Sma,
    pullback: Cross,
    bar: i64,
    /// The low of the last bar that closed down under SMA(200), and its bar.
    down_bar: Option<(f64, i64)>,
    /// The high of the last bar that closed up over SMA(200), and its bar.
    up_bar: Option<(f64, i64)>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: Sma::new(20.0).expect("a valid length"),
        slow: Sma::new(200.0).expect("a valid length"),
        pullback: Cross::new(),
        bar: 0,
        down_bar: None,
        up_bar: None,
    })
}

impl S {
    /// The level of a remembered bar still within the lookback.
    fn within(&self, seen: Option<(f64, i64)>) -> f64 {
        seen.filter(|&(_, at)| self.bar - at <= LOOKBACK).map_or(f64::NAN, |(level, _)| level)
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let fast = self.fast.next(bar);
        let slow = self.slow.next(bar);
        let cross = self.pullback.step(bar.close, fast);
        if bar.close < bar.open && bar.close < slow {
            self.down_bar = Some((bar.low, self.bar));
        }
        if bar.close > bar.open && bar.close > slow {
            self.up_bar = Some((bar.high, self.bar));
        }
        let flat = strategy.position() == 0.0;
        if flat && fast > slow && matches!(cross, Signal::Long) {
            let stop = self.within(self.down_bar) - BUFFER;
            let cut = if stop < bar.close { stop } else { bar.close - FALLBACK };
            strategy.entry("BUY", Long).size(1.0);
            strategy.exit("BUY TP/SL").from("BUY").take(bar.close + TARGET).stop(cut);
        }
        if flat && fast < slow && matches!(cross, Signal::Short) {
            let stop = self.within(self.up_bar) + BUFFER;
            let cut = if stop > bar.close { stop } else { bar.close + FALLBACK };
            strategy.entry("SELL", Short).size(1.0);
            strategy.exit("SELL TP/SL").from("SELL").take(bar.close - TARGET).stop(cut);
        }
        self.bar += 1;
    }
}
