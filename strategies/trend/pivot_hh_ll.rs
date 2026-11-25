// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-liqsweep-pivot-hh-ll-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long on the bar that confirms a pivot high above the previous pivot high, short on the bar that confirms a pivot low below the
//! previous pivot low. A pivot has 5 bars on each side and is confirmed 5 bars after it.
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
    /// Vike's `confirmed_pivot` at 5 bars a side: an older bar may tie and a newer bar must lose strictly, as in
    /// `pivot_confirmed_break`.
    pivots: ConfirmedPivot,
    // NaN until the first pivot of its side, and a NaN compares false.
    last_high: f64,
    last_low: f64,
    // Only a bar firing both sides reads it: the side held turns over. Each entry sets it whatever came before, so the warm-up,
    // which trades nothing, cannot leave it wrong after the first real entry.
    held: i32,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        pivots: ConfirmedPivot::new(5.0, 5.0).expect("valid spans"),
        last_high: f64::NAN,
        last_low: f64::NAN,
        held: 0,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        // The pivots this bar confirms (NaN when none), then the newest of each side.
        let [pivot_high, pivot_low, newest_high, newest_low] = self.pivots.next(bar);
        let higher_high = pivot_high > self.last_high;
        let lower_low = pivot_low < self.last_low;
        self.last_high = newest_high;
        self.last_low = newest_low;

        match (higher_high, lower_low) {
            (true, true) if self.held > 0 => {
                self.held = -1;
                Signal::Short
            }
            (true, _) => {
                self.held = 1;
                Signal::Long
            }
            (_, true) => {
                self.held = -1;
                Signal::Short
            }
            _ => Signal::Hold,
        }
    }
}
