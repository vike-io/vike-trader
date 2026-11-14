// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Strategy MACD vs BB" by 03.freeman, https://www.tradingview.com/script/muzLmNwb/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2019-12-13, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Bollinger bands drawn on a MACD line: the line is SMA(8) less SMA(21) of the close, the bands its 40-bar SMA plus and minus
//! two 40-bar population standard deviations. Long 10,000 when the line crosses up through the lower band, short 10,000 when
//! it crosses down through the upper one; up to five entries a side, and a reversal closes all of the other side. No costs;
//! starts with 100,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Sma, Stddev};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const SIZE: f64 = 10_000.0;

struct S {
    fast: Sma,
    slow: Sma,
    basis: Sma,
    stdev: Stddev,
    lower: Cross,
    upper: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let sma = |n: f64| Sma::new(n).expect("a valid length").with_fresh_sum(true);
    Box::new(S {
        fast: sma(8.0),
        slow: sma(21.0),
        basis: sma(40.0),
        stdev: Stddev::new(40.0).expect("a valid length"),
        lower: Cross::new(),
        upper: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { pyramiding: 5, ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let macd = self.fast.next_value(bar.close) - self.slow.next_value(bar.close);
        let basis = self.basis.next_value(macd);
        let dev = 2.0 * self.stdev.next_value(macd);
        let up = matches!(self.lower.step(macd, basis - dev), Signal::Long);
        let down = matches!(self.upper.step(macd, basis + dev), Signal::Short);
        if up {
            strategy.entry("long", Long).size(SIZE);
        }
        if down {
            strategy.entry("short", Short).size(SIZE);
        }
    }
}
