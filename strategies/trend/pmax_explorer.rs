// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "PMax Explorer" by KivancOzbilgic, https://www.tradingview.com/script/nHGK4Qtp/
// The original's notice: © KivancOzbilgic
// Changed: rewritten by hand in Rust for vike.

//! A profit-maximizer line around EMA(10) of the median price, 3 ATR(10) below it while it trends up and above it while it
//! trends down, each side ratcheting with the average. Long 1 when the average crosses up through that line, short 1 when it
//! crosses down, each reversing the other. No costs; starts with 100,000.
//!
//! The rule's screener reads five other symbols, which feed no order, and its date range (2005 to 9999) holds on every bar.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Atr, Ema, Supertrend};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const MULTIPLIER: f64 = 3.0;

struct S {
    average: Ema,
    atr: Atr,
    stop: Supertrend,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        average: Ema::new(10.0).expect("a valid length"),
        atr: Atr::new(10.0).expect("a valid length"),
        stop: Supertrend::new(10.0, MULTIPLIER)
            .expect("valid parameters")
            .with_previous_bands(true)
            .with_gated_turns(true)
            .with_carry_on_close(true),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let average = self.average.next_value((bar.high + bar.low) / 2.0);
        let atr = self.atr.next(bar);
        let [pmax, _] = self.stop.next_centred(average, average, atr);
        match self.cross.step(average, pmax) {
            Signal::Long => {
                strategy.entry("Long", Long).size(1.0);
            }
            Signal::Short => {
                strategy.entry("Short", Short).size(1.0);
            }
            _ => {}
        }
    }
}
