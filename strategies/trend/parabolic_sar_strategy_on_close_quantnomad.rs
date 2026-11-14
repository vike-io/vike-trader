// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Parabolic SAR Strategy (on close) [QuantNomad]" by QuantNomad, https://www.tradingview.com/script/gK7skk6m/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2019-12-18, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! A parabolic SAR (start 0.02, step 0.02, at most 0.2) that flips on the close: a long trend ends when the close is at or
//! below the last bar's SAR, a short one when it is at or above it. Long 1 on the first bar of a long trend, short 1 on the
//! first bar of a short one, each reversing the other. No costs; starts with 100,000.
//!
//! The rule writes its own SAR, which is vike's `Psar` judged on the close (`Psar::with_reversal_on_close`). The second bar
//! starts it from the first: long when that bar closed above its open, with the SAR at its low, else short with the SAR at its
//! high; that bar is a trend's first too. From then on a trend's extreme is its highest high (lowest low), the factor resets to
//! the start on a flip and grows a step on each new extreme, a flip puts the SAR at the last extreme, and otherwise the SAR
//! moves the factor of the way to this bar's extreme.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Psar;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const START: f64 = 0.02;
const INCREMENT: f64 = 0.02;
const MAXIMUM: f64 = 0.2;

struct S {
    sar: Psar,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let sar =
        Psar::new(INCREMENT, MAXIMUM).and_then(|p| p.with_start(START)).expect("valid factors");
    Box::new(S { sar: sar.with_reversal_on_close(true) })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let was = self.sar.uptrend();
        self.sar.next(bar);
        // A trend's first bar: the one that starts it, or a flip.
        match self.sar.uptrend() {
            Some(true) if was != Some(true) => {
                strategy.entry("long", Long).size(1.0);
            }
            Some(false) if was != Some(false) => {
                strategy.entry("short", Short).size(1.0);
            }
            _ => {}
        }
    }
}
