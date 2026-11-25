// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/na-nz-fixnan-history-chain-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! A score is the 8-bar average of a bar's close-to-close change counted twice: once with a missing change as zero, once with
//! the last known change held over it. Long when the score turns positive, flat when it turns negative. It never shorts.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::Sma;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

struct S {
    previous_close: f64,
    last_known_change: f64,
    average: Sma,
    previous_score: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        previous_close: f64::NAN,
        last_known_change: f64::NAN,
        average: Sma::new(8.0).expect("a valid period"),
        previous_score: f64::NAN,
    }))
}

fn zero_if_missing(x: f64) -> f64 {
    if x.is_nan() { 0.0 } else { x }
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let change = bar.close - std::mem::replace(&mut self.previous_close, bar.close);
        if !change.is_nan() {
            self.last_known_change = change;
        }
        let score = self
            .average
            .next_value(zero_if_missing(change) + zero_if_missing(self.last_known_change));
        let previous = zero_if_missing(std::mem::replace(&mut self.previous_score, score));

        if score > 0.0 && previous <= 0.0 {
            Signal::Long
        } else if score < 0.0 && previous >= 0.0 {
            Signal::Close
        } else {
            Signal::Hold
        }
    }
}
