// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Double Bottom and Top Hunter" by sefakantarcilar, https://www.tradingview.com/script/tkyuZRbN/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2024-08-15, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! A double bottom: this bar's low is the lowest of the last 100 bars and equals the low of the last bar before it that was
//! such a lowest low. Long 100 BTC on one, short 100 BTC on the mirror double top, each reversing the other. No costs; starts
//! with 1,000,000.
//!
//! The rule's closes never fire: each compares the 100-bar highest high (lowest low) with itself, strictly.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Donchian;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const SIZE: f64 = 100.0;

struct S {
    channel: Donchian,
    /// The low (high) of the last bar whose low (high) was the window's lowest (highest).
    last_low: Option<f64>,
    last_high: Option<f64>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        channel: Donchian::new(100.0).expect("a valid length"),
        last_low: None,
        last_high: None,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let [highest, _, lowest] = self.channel.next(bar);
        let mut bottom = false;
        let mut top = false;
        if bar.low == lowest {
            bottom = self.last_low == Some(bar.low);
            self.last_low = Some(bar.low);
        }
        if bar.high == highest {
            top = self.last_high == Some(bar.high);
            self.last_high = Some(bar.high);
        }
        if bottom {
            strategy.entry("long", Long).size(SIZE);
        }
        if top {
            strategy.entry("short", Short).size(SIZE);
        }
    }
}
