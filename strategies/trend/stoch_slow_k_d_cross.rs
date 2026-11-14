// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-stoch-slow-k-d-cross-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when the slow stochastic(14, 3, 3) %K crosses up through %D while below 80, short when it crosses down while above
//! 20.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Cross, Signal, SignalRule, SignalSource};

struct S {
    stochastic: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        stochastic: IndicatorLine::new("stochastic", &[14.0, 3.0, 3.0]),
        cross: Cross::new(),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let lines = self.stochastic.next(bar);
        let (k, d) = (lines[0], lines[1]);
        match self.cross.step(k, d) {
            Signal::Long if k < 80.0 => Signal::Long,
            Signal::Short if k > 20.0 => Signal::Short,
            _ => Signal::Hold,
        }
    }
}
