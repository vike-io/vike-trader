// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/udt-method-windowed-method-chain-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when EMA(9) crosses up through EMA(21) with the close more than 1 standard deviation above SMA(50), both over the last 50
//! closes, flat when it crosses down. An unknown or zero deviation scores 0. It never shorts.
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

struct Snapshot {
    value: f64,
    average: f64,
    deviation: f64,
}

impl Snapshot {
    fn store(&mut self, value: f64, average: f64, deviation: f64) {
        (self.value, self.average, self.deviation) = (value, average, deviation);
    }
    fn distance(&self) -> f64 {
        if self.deviation.is_nan() || self.deviation == 0.0 {
            0.0
        } else {
            (self.value - self.average) / self.deviation
        }
    }
    fn update_and_measure(&mut self, value: f64, average: f64, deviation: f64) -> f64 {
        self.store(value, average, deviation);
        self.distance()
    }
}

struct S {
    snapshot: Snapshot,
    average: IndicatorLine,
    deviation: IndicatorLine,
    fast: IndicatorLine,
    slow: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        snapshot: Snapshot { value: f64::NAN, average: f64::NAN, deviation: f64::NAN },
        average: IndicatorLine::new("sma", &[50.0]),
        deviation: IndicatorLine::new("stddev", &[50.0]),
        fast: IndicatorLine::new("ema", &[9.0]),
        slow: IndicatorLine::new("ema", &[21.0]),
        cross: Cross::new(),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let average = self.average.next(bar)[0];
        let deviation = self.deviation.next(bar)[0];
        let distance = self.snapshot.update_and_measure(bar.close, average, deviation);
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        match self.cross.step(fast, slow) {
            Signal::Long if distance > 1.0 => Signal::Long,
            Signal::Short => Signal::Close,
            _ => Signal::Hold,
        }
    }
}
