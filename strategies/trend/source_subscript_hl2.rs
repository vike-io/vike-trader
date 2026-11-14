// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/input-source-subscript-hl2-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when the bar-to-bar change of the (high + low) / 2 midpoint turns positive after a change that was not positive,
//! short when it turns negative after a change that was not negative.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

struct S {
    /// The midpoint one and two bars back. NaN until seen, and a NaN compares false.
    back: [f64; 2],
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S { back: [f64::NAN; 2] }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let mid = (bar.high + bar.low) / 2.0;
        let [one_back, two_back] = self.back;
        let change = mid - one_back;
        let change_before = one_back - two_back;
        self.back = [mid, one_back];
        if change > 0.0 && change_before <= 0.0 {
            Signal::Long
        } else if change < 0.0 && change_before >= 0.0 {
            Signal::Short
        } else {
            Signal::Hold
        }
    }
}
