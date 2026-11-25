// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Ichimoku Cloud" by Yo_adriiiiaan, https://www.tradingview.com/script/sE2bIlCG/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2019-11-07, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Long all of the account's worth while the close is above both leading lines of the Ichimoku cloud as they stood 26 bars
//! ago, short all of it while below both, each reversing the other. 0.075% commission; starts with 1,000.
//!
//! The lines are midpoints of the highest high and the lowest low: conversion 9 bars, base 26, the first leading line the mean of
//! those two, the second 52 bars.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Ichimoku;
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

struct S {
    /// Its leading lines are the two as they stood 26 bars ago.
    cloud: Ichimoku,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S { cloud: Ichimoku::default() })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.075), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let [_, _, first, second, _] = self.cloud.next(bar);
        if bar.close > first && bar.close > second {
            strategy.entry("long", Long).size(share_of_worth(&sizing, 100.0, bar.close));
        } else if bar.close < first && bar.close < second {
            strategy.entry("short", Short).size(share_of_worth(&sizing, 100.0, bar.close));
        }
    }
}
