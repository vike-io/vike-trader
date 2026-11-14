// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "ATR Parabolic SAR Strategy [QuantNomad]" by QuantNomad, https://www.tradingview.com/script/LKfHdoNv/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2020-07-19, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! A parabolic SAR that steps by its acceleration factor times ATR(14) rather than times the distance to the extreme. Long all
//! of the account's worth on the first bar of an up trend, short all of it on the first bar of a down trend, each reversing
//! the other. No costs; starts with 100,000.
//!
//! The SAR is vike's `Psar` judged on the close and stepping by the ATR (`Psar::with_reversal_on_close`,
//! `Psar::with_atr_step`). It starts on the second bar: up if the first bar closed above its open (the SAR then at its low),
//! down otherwise (at its high). An up trend turns down when the close is at or under the last SAR, a down trend up when the
//! close is at or over it, and the turn puts the SAR at the last extreme. Otherwise the SAR moves toward the price by
//! `af * ATR`, `af` starting at 0.02 on a turn and growing 0.02 to at most 0.2 on each bar that makes a new extreme. Until
//! ATR(14) has a value the bar's true range stands in for it.
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
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const START: f64 = 0.02;
const INCREMENT: f64 = 0.02;
const MAXIMUM: f64 = 0.2;
const ATR_LENGTH: f64 = 14.0;

struct S {
    sar: Psar,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let sar = Psar::new(INCREMENT, MAXIMUM)
        .and_then(|p| p.with_start(START))
        .and_then(|p| p.with_reversal_on_close(true).with_atr_step(ATR_LENGTH))
        .expect("valid factors");
    Box::new(S { sar })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let was = self.sar.uptrend();
        self.sar.next(bar);
        // A trend's first bar: the one that starts it, or a turn.
        match self.sar.uptrend() {
            Some(true) if was != Some(true) => {
                strategy.entry("long", Long).size(share_of_worth(&sizing, 100.0, bar.close));
            }
            Some(false) if was != Some(false) => {
                strategy.entry("short", Short).size(share_of_worth(&sizing, 100.0, bar.close));
            }
            _ => {}
        }
    }
}
