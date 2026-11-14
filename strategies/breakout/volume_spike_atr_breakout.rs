// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-volume-spike-atr-breakout-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when the close rises more than ATR(14) above the previous close on volume above 1.5 times its SMA(20), short on the
//! mirror fall. A long closes on a lower close with volume under that SMA, a short on a higher close with the same.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::Sma;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

const BREAK_IN_ATRS: f64 = 1.0;

struct S {
    volume_average: Sma,
    atr: IndicatorLine,
    previous_close: f64,
    // The position these rules asked for: a signal sees no broker. Whatever it says before trading starts, the first
    // real entry overwrites it, and only the exits read it.
    held: i8,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        volume_average: Sma::new(20.0).expect("a valid period"),
        atr: IndicatorLine::new("atr", &[14.0]),
        previous_close: f64::NAN,
        held: 0,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let average = self.volume_average.next_value(bar.volume);
        let atr = self.atr.next(bar)[0];
        let previous = std::mem::replace(&mut self.previous_close, bar.close);

        let spike = bar.volume > average * 1.5;
        let quiet = bar.volume < average;
        let long = bar.close > previous + atr * BREAK_IN_ATRS && spike;
        let short = bar.close < previous - atr * BREAK_IN_ATRS && spike;
        let exit = (self.held > 0 && bar.close < previous && quiet)
            || (self.held < 0 && bar.close > previous && quiet);

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
