// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-rsi-macd-and-continuous-state-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long while RSI(14) is above 50 and the MACD(12,26,9) line is above its signal, otherwise close. Each test only vetoes, so
//! a NaN vetoes nothing, and closing while flat does nothing.
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
    rsi: IndicatorLine,
    macd: IndicatorLine,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        rsi: IndicatorLine::new("rsi", &[14.0]),
        macd: IndicatorLine::new("macd", &[12.0, 26.0, 9.0]),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let rsi = self.rsi.next(bar)[0];
        let macd = self.macd.next(bar);
        let mut long = true;
        if rsi <= 50.0 {
            long = false;
        }
        if macd[0] <= macd[1] {
            long = false;
        }
        if long { Signal::Long } else { Signal::Close }
    }
}
