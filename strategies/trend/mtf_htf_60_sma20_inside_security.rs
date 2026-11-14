// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/mtf-htf-60-sma20-inside-security-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when the last closed UTC hour's close crosses up through the SMA(20) of hourly closes, close a long when it is below that
//! average. Both sides step on each hour's last 15-minute bar.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::Sma;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::higher::HigherTimeframeBars;
use vike_tactics::signal::{Cross, Signal, SignalRule, SignalSource};

struct S {
    hours: HigherTimeframeBars,
    average: Sma,
    hour_close: f64,
    level: f64,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        hours: HigherTimeframeBars::minutes(60),
        average: Sma::new(20.0).expect("a valid period"),
        hour_close: f64::NAN,
        level: f64::NAN,
        cross: Cross::new(),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        if let Some(hour) = self.hours.push(bar) {
            self.hour_close = hour.close;
            self.level = self.average.next(&hour);
        }
        if matches!(self.cross.step(self.hour_close, self.level), Signal::Long) {
            Signal::Long
        } else if self.hour_close < self.level {
            Signal::Close
        } else {
            Signal::Hold
        }
    }
}
