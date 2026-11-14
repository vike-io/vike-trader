// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "SMC Strategy with Premium, Equilibrium, and Discount Zones" by danieltbolsch, https://www.tradingview.com/script/Zcf69Yqd/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2024-11-06, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Swing levels: the swing high is the high of the last bar that was the highest of its 8, the swing low the low of the last
//! bar that was the lowest of its 8, and the equilibrium their mean. A bar tied with an older one of the 8 is not the highest
//! (or lowest): the older one is. Long 10% of the account's worth when the close is below the equilibrium, above the swing
//! low and above SMA(50) of the close; short 10% when it is above the equilibrium, below the swing high and below SMA(50);
//! each reversing the other. No costs; starts with 1,000,000.
//!
//! The rule also asks the close to be within the 20-bar low and high, which it always is once those 20 bars exist.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Donchian, Sma};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const SHARE: f64 = 10.0;

struct S {
    /// The 7 bars before this one.
    before: Donchian,
    before_high: f64,
    before_low: f64,
    block: Donchian,
    sma: Sma,
    high: f64,
    low: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        before: Donchian::new(7.0).expect("a valid length"),
        before_high: f64::NAN,
        before_low: f64::NAN,
        block: Donchian::new(20.0).expect("a valid length"),
        sma: Sma::new(50.0).expect("a valid length"),
        high: f64::NAN,
        low: f64::NAN,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        // Measured: with a tie going to this bar instead, the count holds and the net is 13810.67.
        if bar.high > self.before_high {
            self.high = bar.high;
        }
        if bar.low < self.before_low {
            self.low = bar.low;
        }
        [self.before_high, _, self.before_low] = self.before.next(bar);
        let [block_high, _, block_low] = self.block.next(bar);
        let sma = self.sma.next(bar);
        let (c, mid) = (bar.close, (self.high + self.low) / 2.0);
        if c < mid && c > self.low && c > sma && c >= block_low {
            strategy.entry("buy", Long).size(share_of_worth(&sizing, SHARE, c));
        }
        if c > mid && c < self.high && c < sma && c <= block_high {
            strategy.entry("sell", Short).size(share_of_worth(&sizing, SHARE, c));
        }
    }
}
