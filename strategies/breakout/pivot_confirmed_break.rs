// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-pivot-confirmed-break-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when the close is above the last confirmed pivot high, short when it is below the last confirmed pivot low. A pivot
//! has 4 bars on each side and is confirmed 4 bars after it.
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
    /// Vike's `confirmed_pivot` at 4 bars a side: an older bar may tie and a newer bar must lose strictly, so the last of two
    /// equal extremes is the pivot.
    pivots: ConfirmedPivot,
    // Which side is held: when the close is above the pivot high AND below the pivot low, only the side not held enters.
    // Entries set it whatever came before, so warm-up signals, which trade nothing, cannot leave it wrong after the first
    // real entry.
    held: i32,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S { pivots: ConfirmedPivot::new(4.0, 4.0).expect("valid spans"), held: 0 }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        // The last confirmed pivot of each side: NaN until the first, and a NaN compares false.
        let [_, _, pivot_high, pivot_low] = self.pivots.next(bar);
        let above = bar.close > pivot_high;
        let below = bar.close < pivot_low;
        match (above, below) {
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
