// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/udt-method-uses-history-globals-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when EMA(9) crosses up through EMA(21), the close is more than 1% above the close 3 bars ago and that older bar's range
//! is positive; flat when it crosses down. It never shorts.
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

const LOOKBACK: usize = 3;

#[derive(Clone, Copy)]
struct Past {
    close: f64,
    high: f64,
    low: f64,
}

impl Past {
    fn change_rate(&self, close: f64) -> f64 {
        if self.close == 0.0 { 0.0 } else { (close - self.close) / self.close }
    }
    fn range(&self) -> f64 {
        self.high - self.low
    }
}

struct S {
    // Oldest first, so the front is the bar `LOOKBACK` ago, and NaN until that many bars have passed.
    history: [Past; LOOKBACK],
    fast: IndicatorLine,
    slow: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        history: [Past { close: f64::NAN, high: f64::NAN, low: f64::NAN }; LOOKBACK],
        fast: IndicatorLine::new("ema", &[9.0]),
        slow: IndicatorLine::new("ema", &[21.0]),
        cross: Cross::new(),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let past = self.history[0];
        self.history.rotate_left(1);
        self.history[LOOKBACK - 1] = Past { close: bar.close, high: bar.high, low: bar.low };

        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        match self.cross.step(fast, slow) {
            Signal::Long if past.change_rate(bar.close) > 0.01 && past.range() > 0.0 => {
                Signal::Long
            }
            Signal::Short => Signal::Close,
            _ => Signal::Hold,
        }
    }
}
