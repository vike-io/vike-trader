// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-rsi14-bands-30-70-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when RSI(14) crosses up through 30, short when it crosses down through 70.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{CrossBands, Signal, SignalRule, SignalSource};

struct S {
    rsi: IndicatorLine,
    bands: CrossBands,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        rsi: IndicatorLine::new("rsi", &[14.0]),
        bands: CrossBands::new(30.0, 70.0),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let rsi = self.rsi.next(bar)[0];
        self.bands.step(rsi)
    }
}
