// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "[DS]Entry_Exit_TRADE.V01-Strategy" by DalmarSantos, https://www.tradingview.com/script/RO4rB2HF/
// The original's notice: © DalmarSantos
// Changed: rewritten by hand in Rust for vike.

//! Long 1 when the 8-bar EMA of `ohlc4` crosses up through DEMA(34) of the close, short 1 when it crosses down, each first
//! closing the other. No costs; starts with 100,000.
//!
//! The rule's other lines (more DEMAs, pivots, Williams %R, MACD, TSI) only draw. Its start in 2021 holds on every bar.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Dema, Ema};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

struct S {
    fast: Ema,
    dema: Dema,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: Ema::new(8.0).expect("a valid length"),
        dema: Dema::new(34.0).expect("a valid length"),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let fast = self.fast.next_value((bar.open + bar.high + bar.low + bar.close) / 4.0);
        let dema = self.dema.next(bar);
        match self.cross.step(fast, dema) {
            Signal::Long => {
                strategy.close("short");
                strategy.entry("long", Long).size(1.0);
            }
            Signal::Short => {
                strategy.close("long");
                strategy.entry("short", Short).size(1.0);
            }
            _ => {}
        }
    }
}
