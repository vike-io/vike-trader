// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Bollinger Band Breakout" by Senthaamizh, https://www.tradingview.com/script/UHsvM5JR/
// The original's notice: © Senthaamizh
// Changed: rewritten by hand in Rust for vike.

//! Long all of the account's worth when the close crosses up through the upper Bollinger band (the 20-bar SMA of the close
//! plus 1.5 population standard deviations); closed when the close crosses down through the lower band. No costs; starts with
//! 100,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Bollinger;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "long";

struct S {
    bands: Bollinger,
    enter: Cross,
    exit: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        bands: Bollinger::new(20.0, 1.5).expect("valid parameters"),
        enter: Cross::new(),
        exit: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let [upper, _, lower] = self.bands.next(bar);
        if matches!(self.enter.step(bar.close, upper), Signal::Long) {
            strategy.entry(NAME, Long).size(share_of_worth(&sizing, 100.0, bar.close));
        }
        if matches!(self.exit.step(bar.close, lower), Signal::Short) {
            strategy.close(NAME);
        }
    }
}
