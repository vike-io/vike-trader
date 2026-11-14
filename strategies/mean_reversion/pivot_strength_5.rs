// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-vcp-pivot-strength-5-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long only. Buy on the bar that confirms a pivot low, close on the bar that confirms a pivot high. A pivot has 5 bars on
//! each side and is confirmed 5 bars after it.
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
    /// Vike's `confirmed_pivot` at 5 bars a side: an older bar may tie and a newer bar must lose strictly, the rule
    /// `pivot_confirmed_break` proved.
    pivots: ConfirmedPivot,
    // Only a bar confirming both pivots reads it: flat buys, long closes. A lone pivot sets it whatever came before, so the
    // warm-up, which trades nothing, cannot leave it wrong for long.
    long: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        pivots: ConfirmedPivot::new(5.0, 5.0).expect("valid spans"),
        long: false,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        // The pivots this bar confirms, NaN when none.
        let [pivot_high, pivot_low, ..] = self.pivots.next(bar);
        let low = !pivot_low.is_nan();
        let high = !pivot_high.is_nan();
        if low && !(high && self.long) {
            self.long = true;
            Signal::Long
        } else if high {
            self.long = false;
            Signal::Close
        } else {
            Signal::Hold
        }
    }
}
