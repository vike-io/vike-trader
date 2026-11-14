// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "KPL Swing Strategy" by ceyhun, https://www.tradingview.com/script/4mz6xvnK/
// The original's notice: © ceyhun
// Changed: rewritten by hand in Rust for vike.

//! A swing level: the 20-bar lowest low while the last breakout was up, the 20-bar highest high otherwise (and before any).
//! A breakout is the close above the previous bar's 20-bar highest high (up) or below its lowest low (down). Long 1 when the
//! close crosses up through the level, short 1 when it crosses down, each reversing the other. No costs; starts with 100,000.
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
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const LENGTH: f64 = 20.0;

struct S {
    range: Donchian,
    /// The previous bar's `(highest high, lowest low)`.
    previous: (f64, f64),
    /// The last breakout, `1` up or `-1` down, `0` before any.
    last: i8,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        range: Donchian::new(LENGTH).expect("a valid length"),
        previous: (f64::NAN, f64::NAN),
        last: 0,
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let [res, _, sup] = self.range.next(bar);
        let (res_previous, sup_previous) = std::mem::replace(&mut self.previous, (res, sup));
        if bar.close > res_previous {
            self.last = 1;
        } else if bar.close < sup_previous {
            self.last = -1;
        }
        let level = if self.last == 1 { sup } else { res };
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
