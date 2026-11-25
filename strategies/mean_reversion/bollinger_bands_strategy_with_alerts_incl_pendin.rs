// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Bollinger Bands Strategy with alerts (incl. pending orders) via TradingConnector to Forex" by Peter_O, https://www.tradingview.com/script/a5CmxEJq/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2021-10-28, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Bollinger bands of the close: SMA(20) and 2 population deviations either side. When the close crosses up through the lower
//! band, a buy stop for 1,000 at that band; when it crosses down through the upper band, a sell stop for 1,000 at that one.
//! Each reverses the other, a side's stop is pulled on the next bar that does not cross again, and the two are one group, so a
//! fill pulls the other. No costs; starts with 1,000,000.
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
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const SIZE: f64 = 1000.0;
const UP: &str = "BBandLE";
const DOWN: &str = "BBandSE";
const GROUP: &str = "BollingerBands";

struct S {
    bands: Bollinger,
    lower: Cross,
    upper: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        bands: Bollinger::new(20.0, 2.0).expect("valid parameters"),
        lower: Cross::new(),
        upper: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let [upper, _, lower] = self.bands.next(bar);
        if matches!(self.lower.step(bar.close, lower), Signal::Long) {
            strategy.entry(UP, Long).size(SIZE).stop(lower).group(GROUP);
        } else {
            strategy.cancel(UP);
        }
        if matches!(self.upper.step(bar.close, upper), Signal::Short) {
            strategy.entry(DOWN, Short).size(SIZE).stop(upper).group(GROUP);
        } else {
            strategy.cancel(DOWN);
        }
    }
}
