// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "UT Bot v2 — ATR Trailing Stop" by QuantNomad, https://www.tradingview.com/script/lDGmh7ou/
// The original's notice: © QuantNomad
// Changed: rewritten by hand in Rust for vike.

//! A trailing level one ATR(10) from the close: while the close and the one before it are both above the previous level, the
//! highest of `close - ATR` since; both below, the lowest of `close + ATR`; otherwise it starts again on the close's side.
//! Long all of the account's worth when the close crosses up through the level, short all of it when it crosses down, each
//! reversing the other. No costs; starts with 100,000. The header states neither size nor capital, which on language
//! version 6 means those two, and a margin of 100%.
//!
//! The level is vike's `AtrTrailingStop` started down (`with_down_start`): the rule compares the close with a level not yet
//! set, which is false, so its first level is `close + ATR`.
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
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

struct S {
    level: AtrTrailingStop,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        level: AtrTrailingStop::new(10.0, 1.0).expect("valid parameters").with_down_start(true),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let src = bar.close;
        let level = self.level.next(bar);
        match self.cross.step(src, level) {
            Signal::Long => {
                strategy.entry("Long", Long).size(share_of_worth(&sizing, 100.0, src));
            }
            Signal::Short => {
                strategy.entry("Short", Short).size(share_of_worth(&sizing, 100.0, src));
            }
            _ => {}
        }
    }
}
