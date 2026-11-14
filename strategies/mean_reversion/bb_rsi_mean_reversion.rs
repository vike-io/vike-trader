// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-bb-rsi-mean-reversion-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when the close is under the lower Bollinger band (20, 2) with RSI(14) under 30, short when it is over the upper band
//! with RSI over 70. A long closes once the close is above the middle band, a short once it is below.
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
    bands: IndicatorLine,
    rsi: IndicatorLine,
    // The position these rules asked for: a signal sees no broker. Whatever it says before trading starts, the first
    // real entry overwrites it, and only the exits read it.
    held: i8,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        bands: IndicatorLine::new("bollinger", &[20.0, 2.0]),
        rsi: IndicatorLine::new("rsi", &[14.0]),
        held: 0,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        // The `bollinger` row's lines are upper, mid, lower.
        let bands = self.bands.next(bar);
        let rsi = self.rsi.next(bar)[0];

        let long = bar.close < bands[2] && rsi < 30.0;
        let short = bar.close > bands[0] && rsi > 70.0;
        let exit =
            (self.held > 0 && bar.close > bands[1]) || (self.held < 0 && bar.close < bands[1]);

        if long {
            self.held = 1;
            Signal::Long
        } else if short {
            self.held = -1;
            Signal::Short
        } else if exit {
            self.held = 0;
            Signal::Close
        } else {
            Signal::Hold
        }
    }
}
