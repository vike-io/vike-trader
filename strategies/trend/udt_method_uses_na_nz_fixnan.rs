// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/udt-method-uses-na-nz-fixnan-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when EMA(9) crosses up through EMA(21) on a bar whose close is above the previous close, flat when it crosses down. The
//! comparison is a ratio to the previous close, and the very first bar compares with itself. It never shorts.
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

struct CloseRatio {
    previous: f64,
}

impl CloseRatio {
    fn step(&mut self, current: f64) -> f64 {
        let base = if self.previous.is_nan() { current } else { self.previous };
        self.previous = current;
        let ratio = if base == 0.0 { 1.0 } else { current / base };
        if ratio.is_nan() { 1.0 } else { ratio }
    }
}

struct S {
    ratio: CloseRatio,
    fast: IndicatorLine,
    slow: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        ratio: CloseRatio { previous: f64::NAN },
        fast: IndicatorLine::new("ema", &[9.0]),
        slow: IndicatorLine::new("ema", &[21.0]),
        cross: Cross::new(),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let ratio = self.ratio.step(bar.close);
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        match self.cross.step(fast, slow) {
            Signal::Long if ratio > 1.0 => Signal::Long,
            Signal::Short => Signal::Close,
            _ => Signal::Hold,
        }
    }
}
