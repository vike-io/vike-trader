// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/analyzer-self-test-multi-mode-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when flat and the 15-minute close crosses up through the high of the day before the last closed UTC day, close when long
//! and the close is more than 0.5% below it. The source offers four entry shapes and this one is chosen, the same rule
//! as `mtf_daily_previous_high_break`.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::higher::HigherTimeframeBars;
use vike_tactics::signal::{Cross, Signal, SignalRule, SignalSource};

struct S {
    days: HigherTimeframeBars,
    last_closed_high: f64,
    level: f64,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        days: HigherTimeframeBars::minutes(1_440),
        last_closed_high: f64::NAN,
        level: f64::NAN,
        cross: Cross::new(),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        if let Some(day) = self.days.push(bar) {
            self.level = self.last_closed_high;
            self.last_closed_high = day.high;
        }
        if matches!(self.cross.step(bar.close, self.level), Signal::Long) {
            Signal::Long
        } else if bar.close < self.level * 0.995 {
            Signal::Close
        } else {
            Signal::Hold
        }
    }
}
