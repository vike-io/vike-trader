// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Scalping Trading System Crypto and Stocks" by exlux, https://www.tradingview.com/script/YotdMm0M/
// The original's notice: © exlux99
// Changed: rewritten by hand in Rust for vike.

//! Every average is of the low. Long 1 when the close is above SMA(25) and EMA(200), inside a Keltner channel (SMA(10) of
//! the low, 2 ATR(14) either side), the MACD histogram (EMA(4) less EMA(34), signal EMA(5)) below 0 and the 10-bar stochastic
//! below 50; short 1 on the mirror (below both averages, inside the channel, histogram above 0, stochastic above 50). Each
//! reverses the other. No costs; starts with 1,000,000.
//!
//! The stochastic's smoothings are both 1, so it is the raw one. The signal EMA starts on the MACD's first number, as vike's `Macd` does.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Atr, Ema, Macd, Sma, Stochastic};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const MULTIPLIER: f64 = 2.0;

struct S {
    average: Sma,
    trend: Ema,
    mid: Sma,
    atr: Atr,
    stochastic: Stochastic,
    macd: Macd,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        average: Sma::new(25.0).expect("a valid length"),
        trend: Ema::new(200.0).expect("a valid length"),
        mid: Sma::new(10.0).expect("a valid length"),
        atr: Atr::new(14.0).expect("a valid length"),
        stochastic: Stochastic::new(10.0, 1.0, 1.0).expect("valid lengths"),
        macd: Macd::new(4.0, 34.0, 5.0).expect("valid lengths"),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let src = bar.low;
        let average = self.average.next_value(src);
        let trend = self.trend.next_value(src);
        let mid = self.mid.next_value(src);
        let range = self.atr.next(bar);
        let (upper, lower) = (mid + range * MULTIPLIER, mid - range * MULTIPLIER);
        let [k, _] = self.stochastic.next(bar);
        let [_, _, hist] = self.macd.next_value(src);
        let close = bar.close;
        let inside = close < upper && close > lower;
        if close > average && inside && hist < 0.0 && k < 50.0 && close > trend {
            strategy.entry("long", Long).size(1.0);
        }
        if close < average && inside && hist > 0.0 && k > 50.0 && close < trend {
            strategy.entry("short", Short).size(1.0);
        }
    }
}
