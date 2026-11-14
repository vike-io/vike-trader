// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/udt-method-var-instance-streak-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long on the fourth green bar in a row or any later one, flat on the first bar that is not green. A green bar closes above its
//! open. It never shorts.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

struct Runs {
    green: u32,
    other: u32,
}

impl Runs {
    fn observe(&mut self, is_green: bool) -> u32 {
        if is_green {
            self.green += 1;
            self.other = 0;
        } else {
            self.other += 1;
            self.green = 0;
        }
        self.green
    }
}

struct S {
    runs: Runs,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S { runs: Runs { green: 0, other: 0 } }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let green = self.runs.observe(bar.close > bar.open);
        if green >= 4 {
            Signal::Long
        } else if self.runs.other >= 1 {
            Signal::Close
        } else {
            Signal::Hold
        }
    }
}
