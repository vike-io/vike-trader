// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Trend Surfers - Breakout + Alerts" by TrendSurfersSignals, https://www.tradingview.com/script/vOfNuQ8n/
// The original's notice: © TrendSurfersSignals
// Changed: rewritten by hand in Rust for vike.

//! Breakouts of the levels as the bar before stood: long all of the account's worth when the close crosses up through the
//! highest high of 100 bars, short when it crosses down through the lowest low of 100. A long closes when the close crosses
//! UP through the lowest high of 50 bars (as written) or on a short entry; a short closes when it crosses up through the
//! highest low of 50 or on a long entry. Each close is issued after its own side's entry and before the other side's, as the
//! rule orders them. The rule's date window covers every bar. 0.1% commission; starts with 2,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Donchian;
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

/// `[highest high of 100, lowest low of 100, lowest high of 50, highest low of 50]`, the bar before's.
type Levels = [f64; 4];

struct S {
    entry: Donchian,
    exit: Donchian,
    before: Levels,
    long_in: Cross,
    long_out: Cross,
    short_in: Cross,
    short_out: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        entry: Donchian::new(100.0).expect("a valid length"),
        exit: Donchian::new(50.0).expect("a valid length"),
        before: [f64::NAN; 4],
        long_in: Cross::new(),
        long_out: Cross::new(),
        short_in: Cross::new(),
        short_out: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.1), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let [long_level, short_level, long_exit, short_exit] = self.before;
        let [highest_high, _, lowest_low] = self.entry.next(bar);
        // Fed the low as the high and the high as the low: its upper line is the highest low, its lower the lowest high.
        let [highest_low, _, lowest_high] = self.exit.next_hl(bar.low, bar.high);
        self.before = [highest_high, lowest_low, lowest_high, highest_low];

        let close = bar.close;
        let enter_long = matches!(self.long_in.step(close, long_level), Signal::Long);
        let exit_long = matches!(self.long_out.step(close, long_exit), Signal::Long);
        let enter_short = matches!(self.short_in.step(close, short_level), Signal::Short);
        let exit_short = matches!(self.short_out.step(close, short_exit), Signal::Long);
        if enter_long {
            strategy.entry("long", Long).size(share_of_worth(&sizing, 100.0, close));
        }
        if exit_long || enter_short {
            strategy.close("long");
        }
        if enter_short {
            strategy.entry("short", Short).size(share_of_worth(&sizing, 100.0, close));
        }
        if exit_short || enter_long {
            strategy.close("short");
        }
    }
}
