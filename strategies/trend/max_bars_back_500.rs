// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-kanuck-max-bars-back-500-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long while the close is above the close 450 bars back, short while it is below. Nothing fires until SMA(400), the RSI(14) of
//! the close 200 bars back and the close 450 bars back all exist.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use std::collections::VecDeque;
use vike_tactics::Rule;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

use vike_indicators::indicators::{Rsi, Sma};
use vike_marketdata::Bar;
use vike_model::params::Params;

const RSI_SOURCE_LAG: usize = 200;
const DEEP_LAG: usize = 450;

struct S {
    // Oldest first, `DEEP_LAG + 1` closes once full.
    closes: VecDeque<f64>,
    long_average: Sma,
    deep_strength: Rsi,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        closes: VecDeque::with_capacity(DEEP_LAG + 1),
        long_average: Sma::new(400.0).expect("a valid period"),
        deep_strength: Rsi::new(14.0).expect("a valid period"),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        if self.closes.len() > DEEP_LAG {
            self.closes.pop_front();
        }
        self.closes.push_back(bar.close);
        let held = self.closes.len();

        let average = self.long_average.next(bar);
        // The RSI's first close is the first lagged close that exists: it must not see the bars before it.
        let strength = if held > RSI_SOURCE_LAG {
            let lagged = self.closes[held - 1 - RSI_SOURCE_LAG];
            self.deep_strength.next(&Bar { close: lagged, ..bar.clone() })
        } else {
            f64::NAN
        };
        if average.is_nan() || strength.is_nan() || held <= DEEP_LAG {
            return Signal::Hold;
        }

        let deep_close = self.closes[0];
        if bar.close > deep_close {
            Signal::Long
        } else if bar.close < deep_close {
            Signal::Short
        } else {
            Signal::Hold
        }
    }
}
