// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "UT Bot Strategy" by QuantNomad, https://www.tradingview.com/script/VKMgZdYr/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2019-11-15, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! An ATR trailing level of the close, 1 ATR(10) away (vike's `atr_trailing_stop`): it ratchets toward the close while the
//! close stays on one side of it two bars running, and jumps to the other side when the close crosses it. Long 1 when the close
//! crosses up through the level, short 1 when it crosses down, each reversing the other. No costs; starts with 100,000.
//!
//! The rule's level starts from 0 (its `nz`) once the ATR has a value, which makes its first value `close - loss`, as vike's
//! does. The rule's Heikin-Ashi source is off by default, so it reads the plain close.
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
        let level = self.stop.next(bar);
        match self.cross.step(bar.close, level) {
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
