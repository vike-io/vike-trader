// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "SSL Channel Cross" by Hegnar, https://www.tradingview.com/script/jds0DHEd/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2019-09-11, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! The SSL channel: the 10-bar SMAs of the high and of the low, with a side that turns up when the close goes above the high
//! average and down when it goes below the low one, and stays put in between. Turned up, the up line is the high average and
//! the down line the low one; turned down, the two swap. Long 1 when the up line crosses up through the down line, short 1
//! when it crosses down, each closing the other first. No costs; starts with 100,000.
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

const LENGTH: f64 = 10.0;

struct S {
    high: Sma,
    low: Sma,
    /// `1` up, `-1` down, `0` before either.
    side: i8,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        high: Sma::new(LENGTH).expect("a valid length"),
        low: Sma::new(LENGTH).expect("a valid length"),
        side: 0,
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let high = self.high.next_value(bar.high);
        let low = self.low.next_value(bar.low);
        if bar.close > high {
            self.side = 1;
        } else if bar.close < low {
            self.side = -1;
        }
        let (down, up) = if self.side < 0 { (high, low) } else { (low, high) };
        match self.cross.step(up, down) {
            Signal::Long => {
                strategy.close("short");
                strategy.entry("long", Long).size(1.0);
            }
            Signal::Short => {
                strategy.close("long");
                strategy.entry("short", Short).size(1.0);
            }
            _ => {}
        }
    }
}
