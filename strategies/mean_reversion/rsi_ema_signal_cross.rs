// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-rsi-ema-signal-cross-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when RSI(14) crosses up through its EMA(9) with RSI below 50, short when it crosses down through its EMA(9) with
//! RSI above 50.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::Ema;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Cross, Signal, SignalRule, SignalSource};

struct S {
    rsi: IndicatorLine,
    average: Ema,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        rsi: IndicatorLine::new("rsi", &[14.0]),
        // The average seeds from the first defined values: the RSI's warm-up NaNs are skipped, not pushed.
        average: Ema::new(9.0).expect("a valid period").with_skip_nan(true),
        cross: Cross::new(),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let rsi = self.rsi.next(bar)[0];
        let average = self.average.next_value(rsi);
        match self.cross.step(rsi, average) {
            Signal::Long if rsi < 50.0 => Signal::Long,
            Signal::Short if rsi > 50.0 => Signal::Short,
            _ => Signal::Hold,
        }
    }
}
