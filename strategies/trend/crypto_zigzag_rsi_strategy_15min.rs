// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Crypto ZigZag RSI strategy 15min" by SoftKill21, https://www.tradingview.com/script/LhpfQMtP/
// The original's notice: © SoftKill21
// Changed: rewritten by hand in Rust for vike.

//! A zigzag of signal closes: the close of the last bar on which RSI(5) crossed up through 25 is the high candidate, the
//! close of the last one on which it crossed down through 75 the low candidate, each 0 until it first happens. The trend
//! starts up with both extremes at those candidates. Up, a higher high candidate raises the high, and a low candidate more
//! than 1% under the high turns it down with the low there; down, a lower low candidate lowers the low, and a high candidate
//! more than 1% over the low turns it up with the high at the LOW candidate (as written). Long 1 when it turns up, closed
//! when it turns down. No costs; starts with 100,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Rsi;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "long";
const OVERSOLD: f64 = 25.0;
const OVERBOUGHT: f64 = 75.0;
const ZIGZAG_PERCENT: f64 = 1.0;

struct S {
    rsi: Rsi,
    up_through: Cross,
    down_through: Cross,
    high_candidate: f64,
    low_candidate: f64,
    /// `None` before the first bar.
    trend: Option<f64>,
    high: f64,
    low: f64,
    turn: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        rsi: Rsi::new(5.0).expect("a valid length"),
        up_through: Cross::new(),
        down_through: Cross::new(),
        high_candidate: 0.0,
        low_candidate: 0.0,
        trend: None,
        high: 0.0,
        low: 0.0,
        turn: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let rsi = self.rsi.next(bar);
        if matches!(self.up_through.step(rsi, OVERSOLD), Signal::Long) {
            self.high_candidate = bar.close;
        }
        if matches!(self.down_through.step(rsi, OVERBOUGHT), Signal::Short) {
            self.low_candidate = bar.close;
        }
        let (r1, s1) = (self.high_candidate, self.low_candidate);
        let mut trend = match self.trend {
            Some(t) => t,
            None => {
                (self.low, self.high) = (s1, r1);
                1.0
            }
        };
        let pi = ZIGZAG_PERCENT * 0.01;
        if trend > 0.0 {
            if r1 >= self.high {
                self.high = r1;
            } else if s1 < self.high * (1.0 - pi) {
                trend = -1.0;
                self.low = s1;
            }
        } else if s1 <= self.low {
            self.low = s1;
        } else if r1 > self.low * (1.0 + pi) {
            trend = 1.0;
            self.high = s1;
        }
        self.trend = Some(trend);
        match self.turn.step(trend, 0.0) {
            Signal::Long => {
                strategy.entry(NAME, Long).size(1.0);
            }
            Signal::Short => {
                strategy.close(NAME);
            }
            _ => {}
        }
    }
}
