// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/mtf-triple-tf-close-confluence-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when a bar closes a UTC hour, a UTC 4-hour bar and a UTC day, each above the close of the one before it; short when all
//! three close below. Only a day's last bar closes all three, so that is the only bar that can trade.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::higher::HigherTimeframeBars;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

struct Timeframe {
    bars: HigherTimeframeBars,
    last_close: f64,
}

impl Timeframe {
    fn new(minutes: i64) -> Self {
        Timeframe { bars: HigherTimeframeBars::minutes(minutes), last_close: f64::NAN }
    }
    /// How far this chart bar moved the timeframe's held close: 0 when it closes no higher bar, NaN on the first close.
    fn step(&mut self, bar: &Bar) -> f64 {
        let Some(closed) = self.bars.push(bar) else {
            return 0.0;
        };
        let moved = closed.close - self.last_close;
        self.last_close = closed.close;
        moved
    }
}

struct S {
    timeframes: [Timeframe; 3],
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S { timeframes: [60, 240, 1_440].map(Timeframe::new) }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let moved = [
            self.timeframes[0].step(bar),
            self.timeframes[1].step(bar),
            self.timeframes[2].step(bar),
        ];
        if moved.iter().all(|m| *m > 0.0) {
            Signal::Long
        } else if moved.iter().all(|m| *m < 0.0) {
            Signal::Short
        } else {
            Signal::Hold
        }
    }
}
