// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Donchain BO" by Senthaamizh, https://www.tradingview.com/script/fF9nC8iP/
// The original's notice: © Senthaamizh
// Changed: rewritten by hand in Rust for vike.

//! Long all of the account's worth when the close crosses up through the last bar's 20-bar highest high; closed when it
//! crosses down through the last bar's 20-bar lowest low. No costs; starts with 100,000.
//!
//! The rule's other exit, through the channel's middle, is an input left at the lower band.
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
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "long";

struct S {
    channel: Donchian,
    /// The last bar's `[upper, lower]`.
    last: [f64; 2],
    enter: Cross,
    exit: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        channel: Donchian::new(20.0).expect("a valid length"),
        last: [f64::NAN; 2],
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
        let [upper, _, lower] = self.channel.next(bar);
        let up = matches!(self.enter.step(bar.close, self.last[0]), Signal::Long);
        let down = matches!(self.exit.step(bar.close, self.last[1]), Signal::Short);
        self.last = [upper, lower];
        if up {
            strategy.entry(NAME, Long).size(share_of_worth(&sizing, 100.0, bar.close));
        }
        if down {
            strategy.close(NAME);
        }
    }
}
