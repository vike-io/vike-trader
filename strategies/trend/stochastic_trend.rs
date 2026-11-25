// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Stochastic Trend" by FieryTrading, https://www.tradingview.com/script/hk9a7099/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2021-04-18, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Long all of the account's worth when the 100-bar SMA of the 200-bar stochastic crosses up through 50; closed when it
//! crosses down through 60. 0.2% commission; starts with 100,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Stochastic;
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "long";
const ENTER: f64 = 50.0;
const EXIT: f64 = 60.0;

struct S {
    stochastic: Stochastic,
    enter: Cross,
    exit: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        stochastic: Stochastic::new(200.0, 100.0, 3.0).expect("valid lengths"),
        enter: Cross::new(),
        exit: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.2), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let [k, _] = self.stochastic.next(bar);
        if matches!(self.enter.step(k, ENTER), Signal::Long) {
            strategy.entry(NAME, Long).size(share_of_worth(&sizing, 100.0, bar.close));
        }
        if matches!(self.exit.step(k, EXIT), Signal::Short) {
            strategy.close(NAME);
        }
    }
}
