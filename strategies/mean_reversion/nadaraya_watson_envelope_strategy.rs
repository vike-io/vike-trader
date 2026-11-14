// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Nadaraya-Watson Envelope Strategy" by Julien_Exe, https://www.tradingview.com/script/HrZicISx/
// The original's notice: © Julien_Eche
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2023-07-21, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Long 20% of the account's worth when the close crosses up through the lower envelope; closed when the upper envelope crosses
//! up through the close. No costs; starts with 1,000,000. The rule's default is long only, and its ATR bands are drawn, not
//! traded.
//!
//! Each envelope is `exp` of a kernel average of the log of the high (upper) or the low (lower) over this bar and the 8 before
//! it, bar `i` back weighted `(1 + (25 - i)^2 / (2 * 8 * 8^2))^-8`, the weights and the products summed from this bar back.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;

const NAME: &str = "long";
const LOOKBACK: usize = 8;
const WEIGHTING: f64 = 8.0;
const START_AT: f64 = 25.0;

/// The rule's kernel average over the last `LOOKBACK + 1` values, newest first; NaN until it has them.
fn kernel(history: &VecDeque<f64>) -> f64 {
    if history.len() <= LOOKBACK {
        return f64::NAN;
    }
    let h = LOOKBACK as f64;
    let (mut weights, mut weighted) = (0.0, 0.0);
    for (i, x) in history.iter().take(LOOKBACK + 1).enumerate() {
        let d = START_AT - i as f64;
        let weight = libm::pow(1.0 + d * d / (2.0 * WEIGHTING * h * h), -WEIGHTING);
        weights += weight;
        weighted += weight * x;
    }
    weighted / weights
}

struct S {
    highs: VecDeque<f64>,
    lows: VecDeque<f64>,
    enter: Cross,
    exit: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        highs: VecDeque::new(),
        lows: VecDeque::new(),
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
        for (history, x) in [(&mut self.highs, bar.high), (&mut self.lows, bar.low)] {
            history.push_front(libm::log(x));
            history.truncate(LOOKBACK + 1);
        }
        let upper = libm::exp(kernel(&self.highs));
        let lower = libm::exp(kernel(&self.lows));
        if matches!(self.enter.step(bar.close, lower), Signal::Long) {
            strategy.entry(NAME, Long).size(share_of_worth(&sizing, 20.0, bar.close));
        }
        if matches!(self.exit.step(upper, bar.close), Signal::Long) {
            strategy.close(NAME);
        }
    }
}
