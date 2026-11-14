// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-kkb-kalman-filter-1d-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when the close crosses up through a one-dimensional Kalman filter of the close (process variance 0.001, measurement
//! variance 0.1), short when it crosses down.
//!
//! The filter begins on the first bar's close and remembers that start for hundreds of bars.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::Kalman;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Cross, Signal, SignalRule, SignalSource};

const PROCESS_VARIANCE: f64 = 0.001;
const MEASUREMENT_VARIANCE: f64 = 0.1;

/// vike's `Kalman` with this rule's variances (it starts on the first close, error variance 1). Also copied into `combo_4`.
fn filter() -> Kalman {
    Kalman::new(PROCESS_VARIANCE, MEASUREMENT_VARIANCE).expect("valid variances")
}

struct S {
    filter: Kalman,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S { filter: filter(), cross: Cross::new() }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let estimate = self.filter.next_value(bar.close);
        self.cross.step(bar.close, estimate)
    }
}
