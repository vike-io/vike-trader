// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-vwma-vs-sma-divergence-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when VWMA(20) minus SMA(20) crosses up through zero, short when it crosses down.
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
    vwma: IndicatorLine,
    sma: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        vwma: IndicatorLine::new("vwma", &[20.0]),
        sma: IndicatorLine::new("sma", &[20.0]),
        cross: Cross::new(),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let vwma = self.vwma.next(bar)[0];
        let sma = self.sma.next(bar)[0];
        self.cross.step(vwma - sma, 0.0)
    }
}
