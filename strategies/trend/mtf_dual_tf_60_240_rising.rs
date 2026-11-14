// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/mtf-dual-tf-60-240-rising-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when flat on a bar that closes both a UTC hour and a UTC 4-hour bar, each above the close of the one before it. Close a long
//! on any bar where the hourly close did not just rise, and that includes every bar that closes no hour.
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

/// Whether this chart bar closes a higher bar above the close of the one before it. A NaN previous close compares false.
fn rose(higher: &mut HigherTimeframeBars, last_closed: &mut f64, bar: &Bar) -> bool {
    let Some(closed) = higher.push(bar) else {
        return false;
    };
    let up = closed.close > *last_closed;
    *last_closed = closed.close;
    up
}

struct S {
    hours: HigherTimeframeBars,
    four_hours: HigherTimeframeBars,
    hour_close: f64,
    four_hour_close: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        hours: HigherTimeframeBars::minutes(60),
        four_hours: HigherTimeframeBars::minutes(240),
        hour_close: f64::NAN,
        four_hour_close: f64::NAN,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let hour_up = rose(&mut self.hours, &mut self.hour_close, bar);
        let four_hour_up = rose(&mut self.four_hours, &mut self.four_hour_close, bar);
        if !hour_up {
            Signal::Close
        } else if four_hour_up {
            Signal::Long
        } else {
            Signal::Hold
        }
    }
}
