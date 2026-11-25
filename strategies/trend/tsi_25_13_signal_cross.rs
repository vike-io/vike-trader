// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-tsi-25-13-signal-cross-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when the true strength index crosses up through its EMA(13), short when it crosses down. The index smooths the
//! close-to-close change with an EMA(13) first and an EMA(25) second.
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
    tsi: IndicatorLine,
    average: Ema,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        // The `tsi` row's parameters are `long`, the first smoothing, then `short`, the second.
        tsi: IndicatorLine::new("tsi", &[13.0, 25.0]),
        // The average seeds from the first defined values: the index's warm-up NaNs are skipped, not pushed.
        average: Ema::new(13.0).expect("a valid period").with_skip_nan(true),
        cross: Cross::new(),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let tsi = self.tsi.next(bar)[0];
        let average = self.average.next_value(tsi);
        self.cross.step(tsi, average)
    }
}
