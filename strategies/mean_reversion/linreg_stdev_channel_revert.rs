// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-linreg-stdev-channel-revert-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Mean reversion around the 50-bar regression line of the close, with bands 2 population standard deviations of the close
//! away. Long when the close is under the lower band and above the previous close, short when it is over the upper band
//! and under the previous close. A long closes when the close is above the line, a short when it is below, and an entry on
//! that bar wins over the close.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

struct S {
    line: IndicatorLine,
    deviation: IndicatorLine,
    /// NaN until seen, and a NaN compares false.
    previous_close: f64,
    /// The position this bar opened with: an order sent on a bar fills at the next bar's open.
    held: i32,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        line: IndicatorLine::new("linearreg", &[50.0]),
        deviation: IndicatorLine::new("stddev", &[50.0]),
        previous_close: f64::NAN,
        held: 0,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let line = self.line.next(bar)[0];
        let band = self.deviation.next(bar)[0] * 2.0;
        let close = bar.close;
        let long = close < line - band && close > self.previous_close;
        let short = close > line + band && close < self.previous_close;
        self.previous_close = close;
        let signal = if long {
            Signal::Long
        } else if short {
            Signal::Short
        } else if (self.held > 0 && close > line) || (self.held < 0 && close < line) {
            Signal::Close
        } else {
            Signal::Hold
        };
        match signal {
            Signal::Long => self.held = 1,
            Signal::Short => self.held = -1,
            Signal::Close => self.held = 0,
            Signal::Hold => {}
        }
        signal
    }
}
