// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-marketshift-pivot-state-machine-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! The state turns bullish on a close above the last confirmed pivot high, else bearish on a close below the last confirmed pivot
//! low, and otherwise stays. Long while bullish, short while bearish. A pivot has 5 bars on each side and is confirmed 5 bars
//! after it.
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

/// The state alone, also copied into `combo_6` and `state_edge_detector`.
struct PivotState {
    pivots: ConfirmedPivot,
    state: i32,
}

impl PivotState {
    fn new() -> Self {
        PivotState { pivots: ConfirmedPivot::new(5.0, 5.0).expect("valid spans"), state: 0 }
    }
    /// 1 bullish, -1 bearish, 0 before the first break.
    fn step(&mut self, bar: &Bar) -> i32 {
        // The newest pivot of each side: NaN until the first, and a NaN compares false.
        let [_, _, last_high, last_low] = self.pivots.next(bar);
        if bar.close > last_high {
            self.state = 1;
        } else if bar.close < last_low {
            self.state = -1;
        }
        self.state
    }
}

struct S {
    state: PivotState,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S { state: PivotState::new() }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        match self.state.step(bar) {
            1 => Signal::Long,
            -1 => Signal::Short,
            _ => Signal::Hold,
        }
    }
}
