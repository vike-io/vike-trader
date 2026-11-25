// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/mtf-htf-60-close-change-baseline-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long on the bar where the last closed UTC hour's close differs from the hour before it, close on the bar after. An hour closing
//! at the same price as the one before takes no trade.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::higher::HigherTimeframeBars;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

struct S {
    hours: HigherTimeframeBars,
    last_closed: f64,
    changed_before: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        hours: HigherTimeframeBars::minutes(60),
        last_closed: f64::NAN,
        changed_before: false,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let mut changed = false;
        if let Some(hour) = self.hours.push(bar) {
            changed = !self.last_closed.is_nan() && hour.close != self.last_closed;
            self.last_closed = hour.close;
        }
        let signal = if changed {
            Signal::Long
        } else if self.changed_before {
            Signal::Close
        } else {
            Signal::Hold
        };
        self.changed_before = changed;
        signal
    }
}
