// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "KAMA Strategy - Kaufman's Adaptive Moving Average" by cisl22klasjk, https://www.tradingview.com/script/h8cfMnVH/
// The original's notice: © BenHampson
// The original's notice: Copyright (c) 2019-present, Franklin Moormann (cheatcountry)
// "Moving Average Adaptive Filter" in it is under the MIT license; its text, with this notice, is LICENSE-MIT in the strategies folder.
// Changed: rewritten by hand in Rust for vike.

//! Kaufman's adaptive average of the close: the efficiency ratio is the 14-bar move over the sum of the 14 one-bar moves,
//! 0 while that sum is 0 or undefined, the smoothing `(ratio * (2/3 - 2/21) + 2/21)^2`, and the average starts from 0
//! (`Kama::with_zero_seed`, its flat ratio 0). Its change on the bar is the delta. Long all of the account's worth when the
//! delta is positive and above the 14-bar standard deviation of the deltas; closed when it is negative and its size above
//! half that deviation. No costs; starts with 10,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Kama, Stddev};
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "buy";
const LENGTH: usize = 14;

struct S {
    average: Kama,
    kama: f64,
    spread: Stddev,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        average: Kama::new(LENGTH as f64, 2.0, 20.0)
            .and_then(|k| k.with_flat_ratio(0.0))
            .expect("valid parameters")
            .with_zero_seed(true),
        kama: f64::NAN,
        spread: Stddev::new(LENGTH as f64).expect("a valid length"),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let close = bar.close;
        let kama = self.average.next_value(close);
        let delta = kama - self.kama;
        self.kama = kama;

        let sd = self.spread.next_value(delta);
        if delta > 0.0 && delta > sd {
            strategy.entry(NAME, Long).size(share_of_worth(&sizing, 100.0, close));
        }
        if delta < 0.0 && delta.abs() > 0.5 * sd {
            strategy.close(NAME);
        }
    }
}
