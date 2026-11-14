// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-liqsweep-wait-one-continuation-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! `wick_pierce_close_back`'s sweep, taken one bar later: long on the bar after a sweep of the lows when that bar does not sweep
//! the lows again and closes above its open, short on the bar after a sweep of the highs when that bar does not sweep the highs
//! again and closes below its open.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::ConfirmedPivot;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

struct S {
    sweeps: Sweeps,
    // Last bar's sweeps. A wait of one bar is exactly this: a new sweep restarts it and it runs out on the next bar without one.
    swept_low: bool,
    swept_high: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S { sweeps: Sweeps::new(), swept_low: false, swept_high: false }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let (low, high) = self.sweeps.step(bar);
        let waited_low = std::mem::replace(&mut self.swept_low, low);
        let waited_high = std::mem::replace(&mut self.swept_high, high);
        if waited_low && !low && bar.close > bar.open {
            Signal::Long
        } else if waited_high && !high && bar.close < bar.open {
            Signal::Short
        } else {
            Signal::Hold
        }
    }
}

/// The sweep detector, copied from `wick_pierce_close_back`.
struct Sweeps {
    pivots: ConfirmedPivot,
}

impl Sweeps {
    fn new() -> Self {
        Sweeps { pivots: ConfirmedPivot::new(5.0, 5.0).expect("valid spans") }
    }

    /// Whether this bar swept the lows, and whether it swept the highs. A pivot this bar confirms already counts.
    fn step(&mut self, bar: &Bar) -> (bool, bool) {
        // The newest pivot of each side: NaN until the first, and a NaN compares false.
        let [_, _, last_high, last_low] = self.pivots.next(bar);
        (bar.low < last_low && bar.close > last_low, bar.high > last_high && bar.close < last_high)
    }
}
