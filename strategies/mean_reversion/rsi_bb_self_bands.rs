// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-rsi-bb-self-bands-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Bollinger bands (50, 2) around RSI(14) itself. Long when RSI is under its lower band and rising, short when it is over its
//! upper band and falling. A long closes once RSI is above the bands' middle, a short once it is below.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::{Sma, Stddev};
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

/// On a bar whose close did not move RSI has not moved either, but vike's `rsi` can still change in its last bits, which
/// reads as neither rising nor falling. A real move is at least 1e-3.
const SAME_RSI: f64 = 1e-9;

struct S {
    rsi: IndicatorLine,
    middle: Sma,
    deviation: Stddev,
    previous_rsi: f64,
    // The position these rules asked for: a signal sees no broker. Whatever it says before trading starts, the first
    // real entry overwrites it, and only the exits read it.
    held: i8,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        rsi: IndicatorLine::new("rsi", &[14.0]),
        middle: Sma::new(50.0).expect("a valid period"),
        deviation: Stddev::new(50.0).expect("a valid period"),
        previous_rsi: f64::NAN,
        held: 0,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let rsi = self.rsi.next(bar)[0];
        // The bands take only real RSI values, so their windows fill from its first one.
        if rsi.is_nan() {
            return Signal::Hold;
        }
        let middle = self.middle.next_value(rsi);
        let width = self.deviation.next_value(rsi) * 2.0;
        let previous = std::mem::replace(&mut self.previous_rsi, rsi);

        let long = rsi < middle - width && rsi > previous + SAME_RSI;
        let short = rsi > middle + width && rsi < previous - SAME_RSI;
        let exit = (self.held > 0 && rsi > middle) || (self.held < 0 && rsi < middle);

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
