// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Bollinger Bands Filter" by TradeChartist, https://www.tradingview.com/script/GKgxKjlC/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2020-08-25, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Bands one standard deviation around SMA(55) of the close. Long all of the account's worth on the first bar on which a
//! close above the upper band is more recent than a close below the lower one, short all of it on the first bar on which
//! the reverse holds, each closing the other. Nothing fires until both have happened once. No costs; starts with 100,000.
//!
//! The rule's date range (2018 to 9999) holds on every bar here.
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
use vike_tactics::helpers::{BarsSince, fewer};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const LENGTH: f64 = 55.0;
const MULTIPLIER: f64 = 1.0;

struct S {
    basis: Sma,
    dev: Stddev,
    above: BarsSince,
    below: BarsSince,
    long_was: bool,
    short_was: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        basis: Sma::new(LENGTH).expect("a valid length"),
        dev: Stddev::new(LENGTH).expect("a valid length"),
        above: BarsSince::default(),
        below: BarsSince::default(),
        long_was: false,
        short_was: false,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let basis = self.basis.next(bar);
        let dev = MULTIPLIER * self.dev.next(bar);
        let up = self.above.step(bar.close > basis + dev);
        let down = self.below.step(bar.close < basis - dev);
        let long_is = fewer(up, down);
        let short_is = fewer(down, up);
        let long_signal = long_is && !self.long_was;
        let short_signal = short_is && !self.short_was;
        (self.long_was, self.short_was) = (long_is, short_is);
        if long_signal {
            strategy.entry("long", Long).size(share_of_worth(&sizing, 100.0, bar.close));
        }
        if short_signal {
            strategy.close("long");
            strategy.entry("short", Short).size(share_of_worth(&sizing, 100.0, bar.close));
        }
        if long_signal {
            strategy.close("short");
        }
    }
}
