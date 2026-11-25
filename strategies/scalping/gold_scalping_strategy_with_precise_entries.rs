// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Gold Scalping Strategy with Precise Entries" by vishnush326, https://www.tradingview.com/script/wO3Es23F/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2024-10-11, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Long 1 when EMA(50) is above EMA(200), RSI(14) lies from 45 to 55, the close is above EMA(50) and the bar engulfs a
//! down bar (it rises, closes above that bar's close and opens below it); short 1 on the mirror. Each signal issues its
//! side's exit again: a target 2 past this close and a stop one ATR(14) the other way. No costs; starts with 1,000,000.
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

/// The rule's "20 pips" at 0.10 a pip.
const TARGET: f64 = 2.0;

struct S {
    fast: IndicatorLine,
    slow: IndicatorLine,
    atr: IndicatorLine,
    rsi: IndicatorLine,
    /// The last bar's open and close.
    last: (f64, f64),
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: IndicatorLine::new("ema", &[50.0]),
        slow: IndicatorLine::new("ema", &[200.0]),
        atr: IndicatorLine::new("atr", &[14.0]),
        rsi: IndicatorLine::new("rsi", &[14.0]),
        last: (f64::NAN, f64::NAN),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        let rsi = self.rsi.next(bar)[0];
        let (open, close) = std::mem::replace(&mut self.last, (bar.open, bar.close));
        let calm = (45.0..=55.0).contains(&rsi);
        let bull = bar.close > bar.open && close < open && bar.close > close && bar.open < close;
        let bear = bar.close < bar.open && close > open && bar.close < close && bar.open > close;
        if fast > slow && calm && bull && bar.close > fast {
            strategy.entry("Long", Long).size(1.0);
            strategy.exit("long exit").from("Long").take(bar.close + TARGET).stop(bar.close - atr);
        }
        if fast < slow && calm && bear && bar.close < fast {
            strategy.entry("Short", Short).size(1.0);
            strategy
                .exit("short exit")
                .from("Short")
                .take(bar.close - TARGET)
                .stop(bar.close + atr);
        }
    }
}
