// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-liqsweep-wick-pierce-close-back-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when a bar's low pierces the last confirmed pivot low and it closes back above it, short when its high pierces the last
//! confirmed pivot high and it closes back below it. A pivot has 5 bars on each side and is confirmed 5 bars after it.
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

/// The sweep detector, also copied into `wait_one_continuation`.
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

struct S {
    sweeps: Sweeps,
    // Only a bar sweeping both sides reads it: the side held turns over. Each entry sets it whatever came before, so the warm-up,
    // which trades nothing, cannot leave it wrong after the first real entry.
    held: i32,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S { sweeps: Sweeps::new(), held: 0 }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        match self.sweeps.step(bar) {
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
