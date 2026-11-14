// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/udt-method-on-array-element-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when EMA(9) crosses up through EMA(21) with the current RSI(14) above 50 and the mean RSI of the last 5 bars above 45, flat
//! when it crosses down. Each bar's score is 0.1 times its RSI's distance from 50, and 0 while the RSI is unknown. It never shorts.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use std::collections::VecDeque;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_tactics::Rule;
use vike_tactics::signal::{Cross, Signal, SignalRule, SignalSource};

use vike_marketdata::Bar;
use vike_model::params::Params;

struct Reading {
    price: f64,
    rsi: f64,
}

impl Reading {
    fn score(&self) -> f64 {
        if self.price.is_nan() || self.rsi.is_nan() { 0.0 } else { (self.rsi - 50.0) * 0.1 }
    }
}

struct S {
    /// Oldest first, the newest 5.
    readings: VecDeque<Reading>,
    rsi: IndicatorLine,
    fast: IndicatorLine,
    slow: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        readings: VecDeque::new(),
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
        self.readings.push_back(Reading { price: bar.close, rsi });
        if self.readings.len() > 5 {
            self.readings.pop_front();
        }
        let total: f64 = self.readings.iter().map(Reading::score).sum();
        let average = total / self.readings.len() as f64;
        let latest = self.readings.back().map_or(0.0, Reading::score);
        match self.cross.step(fast, slow) {
            Signal::Long if latest > 0.0 && average > -0.5 => Signal::Long,
            Signal::Short => Signal::Close,
            _ => Signal::Hold,
        }
    }
}
