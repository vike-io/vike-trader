// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Combo Backtest 123 Reversal & Statistical Volatility" by HPotter, https://www.tradingview.com/script/3w6l57bw/
// The original's notice: Copyright by HPotter v1.0 31/07/2021
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2021-07-31, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Two votes, each held until it changes. The reversal vote is up when the close rose on each of the last two bars while the
//! 14-bar stochastic of the close is above 50 and below its own 3-bar SMA, down on the mirror (fell twice, stochastic below 50
//! and above its SMA). The volatility vote is up when the 30-bar extreme-value volatility is above 0.005, down when below
//! 0.0016. Long 1 while both are up, short 1 while both are down, each reversing the other, and flat otherwise. No costs; starts
//! with 100,000.
//!
//! The volatility is `0.6 * sqrt(253 / 30)` times the mean of `ln(highest close / lowest close)` and `ln(highest high / lowest
//! low)` over 30 bars, held to 0..=2.99.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Donchian, Stochastic};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const STOCH_LENGTH: f64 = 14.0;
const LEVEL: f64 = 50.0;
const VOL_LENGTH: f64 = 30.0;
const TOP_BAND: f64 = 0.005;
const LOW_BAND: f64 = 0.0016;

struct S {
    /// The raw %K and its SMA.
    stoch: Stochastic,
    closes: Donchian,
    extremes: Donchian,
    /// The last two closes, the older first.
    before: [f64; 2],
    reversal: i32,
    volatility: i32,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        stoch: Stochastic::new(STOCH_LENGTH, 1.0, 3.0).expect("valid lengths").with_fresh_sum(true),
        closes: Donchian::new(VOL_LENGTH).expect("a valid length"),
        extremes: Donchian::new(VOL_LENGTH).expect("a valid length"),
        before: [f64::NAN; 2],
        reversal: 0,
        volatility: 0,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let [fast, slow] = self.stoch.next(bar);
        let [two_back, one_back] = self.before;
        if two_back < one_back && bar.close > one_back && fast < slow && fast > LEVEL {
            self.reversal = 1;
        } else if two_back > one_back && bar.close < one_back && fast > slow && fast < LEVEL {
            self.reversal = -1;
        }
        self.before = [one_back, bar.close];

        let [max_close, _, min_close] = self.closes.next_hl(bar.close, bar.close);
        let [max_high, _, min_low] = self.extremes.next(bar);
        let time = (253.0 / VOL_LENGTH).sqrt();
        let vol = ((0.6 * libm::log(max_close / min_close) * time)
            + (0.6 * libm::log(max_high / min_low) * time))
            * 0.5;
        let vol = vol.clamp(0.0, 2.99);
        if vol > TOP_BAND {
            self.volatility = 1;
        } else if vol < LOW_BAND {
            self.volatility = -1;
        }

        match (self.reversal, self.volatility) {
            (1, 1) => {
                strategy.entry("Long", Long).size(1.0);
            }
            (-1, -1) => {
                strategy.entry("Short", Short).size(1.0);
            }
            _ => {
                strategy.close_all();
            }
        }
    }
}
