// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-cog-10-signal-cross-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when the center of gravity of the last 10 closes crosses up through its EMA(5), short when it crosses down.
//! The center of gravity is `-sum(close[i] * (i + 1)) / sum(close[i])`, `i = 0` the newest close. vike has no such
//! indicator, so it is computed here.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use std::collections::VecDeque;
use vike_tactics::Rule;
use vike_tactics::signal::{Cross, Signal, SignalRule, SignalSource};

use vike_indicators::indicators::Ema;
use vike_marketdata::Bar;
use vike_model::params::Params;

const WINDOW: usize = 10;

struct S {
    /// Newest first.
    closes: VecDeque<f64>,
    average: Ema,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        closes: VecDeque::with_capacity(WINDOW + 1),
        // The average seeds from the first defined values: the center's NaNs before the window is full are skipped.
        average: Ema::new(5.0).expect("a valid period").with_skip_nan(true),
        cross: Cross::new(),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        self.closes.push_front(bar.close);
        self.closes.truncate(WINDOW);
        let cog = if self.closes.len() < WINDOW {
            f64::NAN
        } else {
            let weighted: f64 =
                self.closes.iter().zip(1_i32..).map(|(c, w)| c * f64::from(w)).sum();
            -weighted / self.closes.iter().sum::<f64>()
        };
        let average = self.average.next_value(cog);
        self.cross.step(cog, average)
    }
}
