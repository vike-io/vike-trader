// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-kama-style-efficiency-ratio-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! An adaptive average driven by a 14-bar efficiency ratio between speeds 2 and 30. Long on the bar it turns up, short on the
//! bar it turns down, and it holds the position in between.
//!
//! The average is written here because vike's `kama` is a different series: that one seeds from a close at bar `period - 1`
//! and reads a flat window as ratio 1. This one starts from zero on the first bar, reads a window that is not yet full or is
//! flat as ratio 0, and never forgets the climb from zero.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use std::collections::VecDeque;
use vike_tactics::Rule;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

use vike_marketdata::Bar;
use vike_model::params::Params;

const LENGTH: usize = 14;
const FAST_SPEED: f64 = 2.0 / (2.0 + 1.0);
const SLOW_SPEED: f64 = 2.0 / (30.0 + 1.0);

struct S {
    closes: VecDeque<f64>,
    average: f64,
    was_up: bool,
    was_down: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        closes: VecDeque::with_capacity(LENGTH + 2),
        average: f64::NAN,
        was_up: false,
        was_down: false,
    }))
}

impl S {
    fn efficiency_ratio(&self) -> f64 {
        if self.closes.len() <= LENGTH {
            return 0.0;
        }
        let net = (self.closes[LENGTH] - self.closes[0]).abs();
        let path: f64 = (1..=LENGTH).map(|i| (self.closes[i] - self.closes[i - 1]).abs()).sum();
        if path != 0.0 { net / path } else { 0.0 }
    }
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        self.closes.push_back(bar.close);
        if self.closes.len() > LENGTH + 1 {
            self.closes.pop_front();
        }
        let speed = self.efficiency_ratio() * (FAST_SPEED - SLOW_SPEED) + SLOW_SPEED;
        let smoothing = speed * speed;

        let previous = self.average;
        let from = if previous.is_nan() { 0.0 } else { previous };
        self.average = from + smoothing * (bar.close - from);

        // Against the first bar's NaN both are false.
        let up = self.average > previous;
        let down = self.average < previous;
        let was_up = std::mem::replace(&mut self.was_up, up);
        let was_down = std::mem::replace(&mut self.was_down, down);

        if up && !was_up {
            Signal::Long
        } else if down && !was_down {
            Signal::Short
        } else {
            Signal::Hold
        }
    }
}
