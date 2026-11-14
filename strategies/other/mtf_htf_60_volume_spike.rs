// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/mtf-htf-60-volume-spike-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long while the last closed UTC hour's volume is above twice the SMA(20) of hourly volume, close when it no longer is. Both
//! sides step on each hour's last 15-minute bar.
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
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

struct S {
    hours: HigherTimeframeBars,
    baseline: Sma,
    spiking: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        hours: HigherTimeframeBars::minutes(60),
        baseline: Sma::new(20.0).expect("a valid period"),
        spiking: false,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        if let Some(hour) = self.hours.push(bar) {
            self.spiking = hour.volume > self.baseline.next_value(hour.volume) * 2.0;
        }
        if self.spiking { Signal::Long } else { Signal::Close }
    }
}
