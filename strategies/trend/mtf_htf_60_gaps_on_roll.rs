// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/mtf-htf-60-gaps-on-roll-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when flat and a closed UTC hour's close is above the last hour's, close a long when it is below. The hourly series has a
//! value only on each hour's last 15-minute bar and none between, so nothing can trade on the other bars.
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
    last_close: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S { hours: HigherTimeframeBars::minutes(60), last_close: f64::NAN }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let Some(hour) = self.hours.push(bar) else {
            return Signal::Hold;
        };
        let before = self.last_close;
        self.last_close = hour.close;
        if hour.close > before {
            Signal::Long
        } else if hour.close < before {
            Signal::Close
        } else {
            Signal::Hold
        }
    }
}
