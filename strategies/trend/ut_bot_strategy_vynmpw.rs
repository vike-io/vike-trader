// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "UT Bot Strategy" by QuantNomad, https://www.tradingview.com/script/RHagaXIp/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2020-01-22, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! An ATR trailing stop on the close (1 × ATR(10)): while the close and the one before stay above the last stop it rises to
//! `close - ATR` and never falls, below it the mirror, and a close that crosses it restarts it on the new side. Long 1 when
//! the close crosses up through the stop, short 1 when it crosses down, each reversing the other. No costs; starts with
//! 100,000.
//!
//! The rule's Heikin-Ashi source is off by default, so its higher-timeframe call is never read; its date range (2019 to 2100)
//! holds on every bar here. The stop starts from 0 (its `nz`) once the ATR has a value, which makes its first value
//! `close - loss`: vike's `AtrTrailingStop`.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::AtrTrailingStop;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

struct S {
    stop: AtrTrailingStop,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        stop: AtrTrailingStop::new(10.0, 1.0).expect("valid parameters"),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let stop = self.stop.next(bar);
        match self.cross.step(bar.close, stop) {
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
