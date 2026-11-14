// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "EVWMA VWAP MACD Strategy [QuantNomad]" by QuantNomad, https://www.tradingview.com/script/ZguF6GBo/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2019-11-19, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! The day's VWAP less the 30-bar EVWMA of the close, smoothed by EMA(5) and EMA(20): long 1 when the fast crosses up through
//! the slow, short 1 when it crosses down, each reversing the other. No costs; starts with 100,000.
//!
//! The VWAP is as in `strategies/trend/evwma_vwap_cross_quantnomad.rs`. Both EMAs skip the difference's first NaN bars.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Ema, Vwap, VwapSource};
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

struct S {
    evwma: IndicatorLine,
    vwap: Vwap,
    fast: Ema,
    slow: Ema,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        evwma: IndicatorLine::new("evwma", &[30.0]),
        vwap: Vwap::default().with_source(VwapSource::TypicalDivided),
        fast: Ema::new(5.0).expect("a valid length").with_skip_nan(true),
        slow: Ema::new(20.0).expect("a valid length").with_skip_nan(true),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let diff = self.vwap.next(bar) - self.evwma.next(bar)[0];
        let fast = self.fast.next_value(diff);
        let slow = self.slow.next_value(diff);
        match self.cross.step(fast, slow) {
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
