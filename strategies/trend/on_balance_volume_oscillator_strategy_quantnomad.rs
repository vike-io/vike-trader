// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "On Balance Volume Oscillator Strategy [QuantNomad]" by QuantNomad, https://www.tradingview.com/script/VQjItUaK/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2019-11-13, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! The 20-bar sum of the volume signed by the close's change, over the 20-bar sum of the volume, smoothed by EMA(12) and
//! EMA(26): long 1 when the fast crosses up through the slow, short 1 when it crosses down, each reversing the other. No costs;
//! starts with 100,000.
//!
//! The sums are `vike_tactics::rolling::RollingSum`, the reference's; a bar whose close did not change adds no volume, and the first
//! bar, with no change, is skipped. Both EMAs skip the oscillator's first NaN bars.
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
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::rolling::RollingSum;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const LENGTH: usize = 20;

struct S {
    signed: RollingSum,
    total: RollingSum,
    last_close: f64,
    fast: Ema,
    slow: Ema,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        signed: RollingSum::new(LENGTH),
        total: RollingSum::new(LENGTH),
        last_close: f64::NAN,
        fast: Ema::new(12.0).expect("a valid length").with_skip_nan(true),
        slow: Ema::new(26.0).expect("a valid length").with_skip_nan(true),
        cross: Cross::new(),
    })
}

/// -1, 0 or 1 by the sign of `x`, NaN for NaN.
fn sign(x: f64) -> f64 {
    if x > 0.0 {
        1.0
    } else if x < 0.0 {
        -1.0
    } else if x == 0.0 {
        0.0
    } else {
        f64::NAN
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let signed = self.signed.next(bar.volume * sign(bar.close - self.last_close));
        self.last_close = bar.close;
        let oscillator = signed / self.total.next(bar.volume);
        let fast = self.fast.next_value(oscillator);
        let slow = self.slow.next_value(oscillator);
        match self.cross.step(fast, slow) {
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
