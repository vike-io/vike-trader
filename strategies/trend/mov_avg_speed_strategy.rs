// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Mov Avg Speed Strategy" by TradeStation, https://www.tradingview.com/script/pCxvfqFv/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2023-07-18, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Three votes, each long, short or neither: the 10-bar SMA of the 1-bar rate of change of SMA(50) of the close (above 0
//! long, below short); the 3-bar SMA of the 14-bar stochastic (at or under 20 long, at or over 80 short); and the 9-bar EMA
//! of MACD(12, 26) against its value a bar before (rising long, falling short). Long 1 on two long votes, short 1 on two
//! short votes, each reversing the other. No costs; starts with 1,000,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Ema, Sma, Stochastic};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const OVERSOLD: f64 = 20.0;
const OVERBOUGHT: f64 = 80.0;

struct S {
    avg: Sma,
    previous_average: f64,
    average_roc: Sma,
    stochastic: Stochastic,
    fast: Ema,
    slow: Ema,
    signal: Ema,
    previous_signal: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        avg: Sma::new(50.0).expect("a valid length"),
        previous_average: f64::NAN,
        average_roc: Sma::new(10.0).expect("a valid length"),
        stochastic: Stochastic::new(14.0, 3.0, 3.0).expect("valid lengths"),
        fast: Ema::new(12.0).expect("a valid length"),
        slow: Ema::new(26.0).expect("a valid length"),
        signal: Ema::new(9.0).expect("a valid length").with_skip_nan(true),
        previous_signal: f64::NAN,
    })
}

/// `1` for a long vote, `-1` for a short one, `0` for neither (a NaN is neither).
fn vote(long: bool, short: bool) -> i32 {
    if long {
        1
    } else if short {
        -1
    } else {
        0
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let avg = self.avg.next(bar);
        let roc = 100.0 * (avg - self.previous_average) / self.previous_average;
        self.previous_average = avg;
        // The SMA of the rate of change starts on its first number: a NaN fed in would stay in the running sum.
        let average_roc = if roc.is_nan() { f64::NAN } else { self.average_roc.next_value(roc) };
        let [k, _] = self.stochastic.next(bar);
        let macd = self.fast.next(bar) - self.slow.next(bar);
        let signal = self.signal.next_value(macd);
        let rising = signal > self.previous_signal;
        let falling = signal < self.previous_signal;
        self.previous_signal = signal;
        let votes = [
            vote(rising, falling),
            vote(k <= OVERSOLD, k >= OVERBOUGHT),
            vote(average_roc > 0.0, average_roc < 0.0),
        ];
        if votes.iter().filter(|&&v| v == 1).count() >= 2 {
            strategy.entry("long", Long).size(1.0);
        }
        if votes.iter().filter(|&&v| v == -1).count() >= 2 {
            strategy.entry("short", Short).size(1.0);
        }
    }
}
