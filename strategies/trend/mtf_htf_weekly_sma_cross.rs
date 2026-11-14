// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/mtf-htf-weekly-sma-cross-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when the 15-minute close crosses up through the SMA(10) of weekly closes, short when it crosses down. A week starts on
//! Monday 00:00 UTC, and the average moves only on the bar that closes a week.
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
    weeks: HigherTimeframeBars,
    average: Sma,
    level: f64,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        weeks: HigherTimeframeBars::weeks(),
        average: Sma::new(10.0).expect("a valid period"),
        level: f64::NAN,
        cross: Cross::new(),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        if let Some(week) = self.weeks.push(bar) {
            self.level = self.average.next(&week);
        }
        self.cross.step(bar.close, self.level)
    }
}
