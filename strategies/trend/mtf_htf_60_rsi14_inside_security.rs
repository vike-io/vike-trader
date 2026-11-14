// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/mtf-htf-60-rsi14-inside-security-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when RSI(14) of hourly closes crosses up through 50, close a long when it crosses down through 50. The RSI runs on the
//! closed UTC hours and steps on each hour's last 15-minute bar.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::Rsi;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::higher::HigherTimeframeBars;
use vike_tactics::signal::{Cross, Signal, SignalRule, SignalSource};

struct S {
    hours: HigherTimeframeBars,
    rsi: Rsi,
    last_closed: f64,
    through_50: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        hours: HigherTimeframeBars::minutes(60),
        rsi: Rsi::new(14.0).expect("a valid period"),
        last_closed: f64::NAN,
        through_50: Cross::new(),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        if let Some(hour) = self.hours.push(bar) {
            self.last_closed = self.rsi.next(&hour);
        }
        match self.through_50.step(self.last_closed, 50.0) {
            Signal::Short => Signal::Close,
            other => other,
        }
    }
}
