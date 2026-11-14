// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/recompute-mtf-rsi-macd-bb-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when flat and three readings of closed UTC hours all agree: RSI(14) above 55, MACD(12, 26, 9) histogram above zero, and the
//! hour's close above its SMA(20). Close a long when RSI is below 45, the histogram below zero, or the close below that SMA.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::{Macd, Rsi, Sma};
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::higher::HigherTimeframeBars;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

struct S {
    hours: HigherTimeframeBars,
    rsi: Rsi,
    macd: Macd,
    middle: Sma,
    rsi_value: f64,
    histogram: f64,
    middle_value: f64,
    hour_close: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        hours: HigherTimeframeBars::minutes(60),
        rsi: Rsi::new(14.0).expect("a valid period"),
        macd: Macd::new(12.0, 26.0, 9.0).expect("valid periods"),
        middle: Sma::new(20.0).expect("a valid period"),
        rsi_value: f64::NAN,
        histogram: f64::NAN,
        middle_value: f64::NAN,
        hour_close: f64::NAN,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        if let Some(hour) = self.hours.push(bar) {
            self.rsi_value = self.rsi.next(&hour);
            self.histogram = self.macd.next(&hour)[2];
            self.middle_value = self.middle.next(&hour);
            self.hour_close = hour.close;
        }
        if self.rsi_value > 55.0 && self.histogram > 0.0 && self.hour_close > self.middle_value {
            Signal::Long
        } else if self.rsi_value < 45.0
            || self.histogram < 0.0
            || self.hour_close < self.middle_value
        {
            Signal::Close
        } else {
            Signal::Hold
        }
    }
}
