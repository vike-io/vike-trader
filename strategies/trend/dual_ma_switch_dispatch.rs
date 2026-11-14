// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-dual-ma-switch-dispatch-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when EMA(10) of the close crosses up through EMA(30), short when it crosses down. The rule lets its average be chosen
//! by name; this is its default choice, the EMA.
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
    fast: IndicatorLine,
    slow: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        fast: IndicatorLine::new("ema", &[10.0]),
        slow: IndicatorLine::new("ema", &[30.0]),
        cross: Cross::new(),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        self.cross.step(fast, slow)
    }
}
