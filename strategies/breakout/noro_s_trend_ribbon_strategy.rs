// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Noro's Trend Ribbon Strategy" by ROBO_Trading, https://www.tradingview.com/script/ZsKsLiUU/
// The original's notice: © noro
// Changed: rewritten by hand in Rust for vike.

//! A channel of the 20-bar SMA of the bar's mean price (open, high, low and close over four): its highest and lowest over 20
//! bars. Long all of the account's worth once the mean price is above the previous bar's highest, short all of it once below
//! the previous bar's lowest, holding the last side in between and reversing from the other. 0.1% commission; starts with
//! 100,000.
//!
//! The rule's date range (1900 to 2100) holds on every bar here, so its closing branches never fire.
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
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const LENGTH: f64 = 20.0;

struct S {
    average: Sma,
    channel: Donchian,
    /// The previous bar's `[highest, mid, lowest]` of the average.
    last: [f64; 3],
    trend: i32,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        average: Sma::new(LENGTH).expect("a valid length"),
        channel: Donchian::new(LENGTH).expect("a valid length"),
        last: [f64::NAN; 3],
        trend: 0,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.1), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let price = (bar.open + bar.high + bar.low + bar.close) / 4.0;
        let average = self.average.next_value(price);
        let [highest, _, lowest] = self.last;
        if price > highest {
            self.trend = 1;
        } else if price < lowest {
            self.trend = -1;
        }
        self.last = self.channel.next_hl(average, average);
        match self.trend {
            1 => {
                strategy.entry("long", Long).size(share_of_worth(&sizing, 100.0, bar.close));
            }
            -1 => {
                strategy.entry("short", Short).size(share_of_worth(&sizing, 100.0, bar.close));
            }
            _ => {}
        }
    }
}
