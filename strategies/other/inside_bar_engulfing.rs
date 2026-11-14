// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-inside-bar-engulfing-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long on an up candle whose range sits strictly inside the previous bar's range, short on a down candle that does.
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
    last_high: f64,
    last_low: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S { last_high: f64::NAN, last_low: f64::NAN }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let inside = bar.high < self.last_high && bar.low > self.last_low;
        (self.last_high, self.last_low) = (bar.high, bar.low);
        if inside && bar.close > bar.open {
            Signal::Long
        } else if inside && bar.close < bar.open {
            Signal::Short
        } else {
            Signal::Hold
        }
    }
}
