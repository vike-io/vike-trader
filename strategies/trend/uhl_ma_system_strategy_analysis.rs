// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Uhl MA System - Strategy Analysis" by alexgrover, https://www.tradingview.com/script/4S9Yz3DB/
// The original's notice: © alexgrover
// Changed: rewritten by hand in Rust for vike.

//! Two adaptive averages of the close: a slow one that moves toward SMA(100) and a fast one that moves toward the close, each
//! by `1 - variance / gap²` when the 100-bar variance is below the squared gap, and not at all otherwise. Long 1 when the fast
//! crosses up through the slow, short 1 when it crosses down, each reversing the other. No costs; starts with 100,000.
//!
//! The slow average is NaN until the SMA exists and then starts from the close; the fast one starts from the first close and
//! holds it until the variance exists. The variance is the population one, the standard deviation squared.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Sma, Stddev};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::helpers::nan_to_zero;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const LENGTH: f64 = 100.0;

struct S {
    sma: Sma,
    stdev: Stddev,
    /// The slow average, NaN until the SMA exists.
    slow: f64,
    /// The fast average, NaN before the first bar.
    fast: f64,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        sma: Sma::new(LENGTH).expect("a valid length").with_fresh_sum(true),
        stdev: Stddev::new(LENGTH).expect("a valid length"),
        slow: f64::NAN,
        fast: f64::NAN,
        cross: Cross::new(),
    })
}

/// The weight toward the target: `1 - variance / gap²` when the variance is below the squared gap, else 0. A NaN gap counts
/// as 0 and a NaN variance compares false.
fn weight(variance: f64, gap: f64) -> f64 {
    let squared = nan_to_zero(gap * gap);
    if variance < squared { 1.0 - variance / squared } else { 0.0 }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let close = bar.close;
        let sma = self.sma.next_value(close);
        let sd = self.stdev.next_value(close);
        let variance = sd * sd;
        let or_close = |x: f64| if x.is_nan() { close } else { x };
        let ka = weight(variance, sma - self.slow);
        let kb = weight(variance, close - self.fast);
        // `ka * sma` is NaN while the SMA is, which keeps the slow average NaN until then.
        self.slow = ka * sma + (1.0 - ka) * or_close(self.slow);
        self.fast = kb * close + (1.0 - kb) * or_close(self.fast);
        match self.cross.step(self.fast, self.slow) {
            Signal::Long => {
                strategy.entry("buy", Long).size(1.0);
            }
            Signal::Short => {
                strategy.entry("sell", Short).size(1.0);
            }
            _ => {}
        }
    }
}
