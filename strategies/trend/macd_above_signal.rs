// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-macd-line-gt-signal-continuous-state-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long while the MACD(12,26,9) line is above its signal, otherwise close. A NaN line reads as false, and closing while
//! flat does nothing.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

struct S {
    macd: IndicatorLine,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S { macd: IndicatorLine::new("macd", &[12.0, 26.0, 9.0]) }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let lines = self.macd.next(bar);
        if lines[0] > lines[1] { Signal::Long } else { Signal::Close }
    }
}
