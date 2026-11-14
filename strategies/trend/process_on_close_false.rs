// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-process-on-close-false-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long only: open when EMA(9) crosses up through EMA(21) with RSI(14) below 70, close when EMA(9) crosses down through
//! EMA(21). A NaN RSI reads as false.
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
    rsi: IndicatorLine,
    fast: IndicatorLine,
    slow: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        rsi: IndicatorLine::new("rsi", &[14.0]),
        fast: IndicatorLine::new("ema", &[9.0]),
        slow: IndicatorLine::new("ema", &[21.0]),
        cross: Cross::new(),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let rsi = self.rsi.next(bar)[0];
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        match self.cross.step(fast, slow) {
            Signal::Long if rsi < 70.0 => Signal::Long,
            Signal::Short => Signal::Close,
            _ => Signal::Hold,
        }
    }
}
