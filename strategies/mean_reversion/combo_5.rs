// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-liqsweep-integration-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Structure turns bullish when a newly confirmed pivot high beats the one before it, bearish when a newly confirmed pivot low
//! undercuts the one before it. While bullish, a bar whose low pierces the last pivot low and whose close is back above it arms a
//! long, which fires on the next bar if that bar closes up and does not itself arm. A short is the mirror. A pivot has 5 bars on
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
    last_high: f64,
    last_low: f64,
    // 1 bullish, -1 bearish, 0 before the first qualifying pivot.
    structure: i32,
    armed_long: bool,
    armed_short: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        pivots: ConfirmedPivot::new(5.0, 5.0).expect("valid spans"),
        last_high: f64::NAN,
        last_low: f64::NAN,
        structure: 0,
        armed_long: false,
        armed_short: false,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        // The pivots this bar confirms, NaN when none.
        let [new_high, new_low, ..] = self.pivots.next(bar);
        if !new_high.is_nan() {
            if new_high > self.last_high {
                self.structure = 1;
            }
            self.last_high = new_high;
        }
        // Second, so a bar confirming both a higher high and a lower low ends bearish.
        if !new_low.is_nan() {
            if new_low < self.last_low {
                self.structure = -1;
            }
            self.last_low = new_low;
        }

        let pierced_low = bar.low < self.last_low && bar.close > self.last_low;
        let pierced_high = bar.high > self.last_high && bar.close < self.last_high;
        let arm_long = pierced_low && self.structure > 0;
        let arm_short = pierced_high && self.structure < 0;
        let fire_long = self.armed_long && !arm_long && bar.close > bar.open;
        let fire_short = self.armed_short && !arm_short && bar.close < bar.open;
        (self.armed_long, self.armed_short) = (arm_long, arm_short);

        if fire_long {
            Signal::Long
        } else if fire_short {
            Signal::Short
        } else {
            Signal::Hold
        }
    }
}
