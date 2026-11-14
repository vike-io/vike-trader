// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-supertrend-direction-flip-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long on the bar Supertrend(10, 3) turns up, short on the bar it turns down. An entry reverses the opposite position.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

struct S {
    supertrend: IndicatorLine,
    previous_direction: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        supertrend: IndicatorLine::new("supertrend", &[10.0, 3.0]),
        previous_direction: f64::NAN,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        // The `supertrend` row's lines are the band and the direction, +1 up and -1 down.
        let direction = self.supertrend.next(bar)[1];
        let previous = std::mem::replace(&mut self.previous_direction, direction);
        if direction > 0.0 && previous < 0.0 {
            Signal::Long
        } else if direction < 0.0 && previous > 0.0 {
            Signal::Short
        } else {
            Signal::Hold
        }
    }
}
