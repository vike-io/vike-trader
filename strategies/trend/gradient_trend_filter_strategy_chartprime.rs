// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from the script by PineIndicators, https://www.tradingview.com/script/Va06tXWV/
// The original's notice: © ChartPrime & PineIndicators
// Changed: rewritten by hand in Rust for vike.

//! A filter of the close: three exponential smoothings in a row with `alpha = 2 / 26`, each started from 0
//! (`Ema::with_zero_seed`). Long all of the account's worth when the filter's two-bar change crosses up through 0, closed
//! when it crosses down. 0.01% commission, 2 ticks of slippage; starts with 10,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Ema;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const LENGTH: f64 = 25.0;

struct S {
    stages: [Ema; 3],
    /// The filter one and two bars back.
    back: [f64; 2],
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let stage = || Ema::new(LENGTH).expect("a valid length").with_zero_seed(true);
    Box::new(S { stages: [stage(), stage(), stage()], back: [f64::NAN; 2], cross: Cross::new() })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.01), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let base = self.stages.iter_mut().fold(bar.close, |x, stage| stage.next_value(x));
        let diff = base - self.back[1];
        self.back = [base, self.back[0]];
        match self.cross.step(diff, 0.0) {
            Signal::Long => {
                strategy.entry("L", Long).size(share_of_worth(&sizing, 100.0, bar.close));
            }
            Signal::Short => {
                strategy.close("L");
            }
            _ => {}
        }
    }
}
