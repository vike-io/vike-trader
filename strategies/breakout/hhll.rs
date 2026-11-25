// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "HHLL" by HPotter, https://www.tradingview.com/script/CRNp5xev/
// The original's notice: Copyright by HPotter v2.0 19/09/2022
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2022-09-19, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Bollinger bands of the typical price (29 bars, 2 deviations), each pushed out by half the bands' width. Set to trade the
//! reverse: long 1 once the high breaks above the previous bar's outer upper band, short 1 once the low breaks below the
//! previous bar's outer lower band (the low winning a bar that does both), holding the last side in between and reversing from
//! the other. No costs; starts with 1,000,000.
//!
//! The rule's start date (2018) holds on every bar here.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Bollinger;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

struct S {
    bands: Bollinger,
    /// The previous bar's outer `(upper, lower)`.
    last: (f64, f64),
    /// The side the bands give before the reversal: 1 after a break below, -1 after a break above.
    side: i32,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        bands: Bollinger::new(29.0, 2.0).expect("valid lengths"),
        last: (f64::NAN, f64::NAN),
        side: 0,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let (upper, lower) = self.last;
        if bar.low < lower {
            self.side = i32::from(Long);
        } else if bar.high > upper {
            self.side = i32::from(Short);
        }
        let typical = Bar { close: (bar.high + bar.low + bar.close) / 3.0, ..bar.clone() };
        let [up, _, down] = self.bands.next(&typical);
        let half = (up - down) / 2.0;
        self.last = (up + half, down - half);
        match -self.side {
            1 => {
                strategy.entry("long", Long).size(1.0);
            }
            -1 => {
                strategy.entry("short", Short).size(1.0);
            }
            _ => {}
        }
    }
}
