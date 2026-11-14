// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/udt-method-in-for-loop-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when the mean of the last 5 closes is above EMA(21) and the close crosses up through that EMA, flat when the close crosses
//! down through it. The up cross is only looked at on bars whose mean is above the EMA, so its "previous bar" is the last such
//! bar, not always the one before. It never shorts.
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

struct Tally {
    sum: f64,
    count: u32,
}

impl Tally {
    fn clear(&mut self) {
        self.sum = 0.0;
        self.count = 0;
    }
    fn add(&mut self, value: f64) {
        self.sum += value;
        self.count += 1;
    }
    fn mean(&self) -> f64 {
        if self.count > 0 { self.sum / f64::from(self.count) } else { 0.0 }
    }
}

struct S {
    /// Newest first. An unseen close is NaN, so the mean is NaN until the fifth bar.
    closes: [f64; 5],
    tally: Tally,
    ema: IndicatorLine,
    gated_up: Cross,
    down: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        closes: [f64::NAN; 5],
        tally: Tally { sum: 0.0, count: 0 },
        ema: IndicatorLine::new("ema", &[21.0]),
        gated_up: Cross::new(),
        down: Cross::new(),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        self.closes.rotate_right(1);
        self.closes[0] = bar.close;
        self.tally.clear();
        for close in self.closes {
            self.tally.add(close);
        }
        let mean = self.tally.mean();
        let ema = self.ema.next(bar)[0];
        let crossed_down = matches!(self.down.step(bar.close, ema), Signal::Short);
        if mean > ema && matches!(self.gated_up.step(bar.close, ema), Signal::Long) {
            Signal::Long
        } else if crossed_down {
            Signal::Close
        } else {
            Signal::Hold
        }
    }
}
