// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "EVWMA VWAP Cross [QuantNomad]" by QuantNomad, https://www.tradingview.com/script/QPIGo8hs/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2019-11-21, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Long 1 when the day's VWAP crosses up through the 30-bar EVWMA of the close, short 1 when it crosses down, each reversing
//! the other. No costs; starts with 100,000.
//!
//! The rule smooths the VWAP with an EMA of length 1, which is the VWAP itself. Its VWAP weights the typical price divided by 3
//! (`VwapSource::TypicalDivided`), restarting each UTC day.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Vwap, VwapSource};
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

struct S {
    evwma: IndicatorLine,
    vwap: Vwap,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        evwma: IndicatorLine::new("evwma", &[30.0]),
        vwap: Vwap::default().with_source(VwapSource::TypicalDivided),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let evwma = self.evwma.next(bar)[0];
        let vwap = self.vwap.next(bar);
        match self.cross.step(vwap, evwma) {
            Signal::Long => {
                strategy.entry("long", Long).size(1.0);
            }
            Signal::Short => {
                strategy.entry("short", Short).size(1.0);
            }
            _ => {}
        }
    }
}
