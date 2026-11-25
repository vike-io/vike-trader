// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-sar-flip-entry-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when the parabolic SAR(start 0.02, step 0.02, cap 0.2) moves from at-or-above the close to below it, short on the
//! opposite move.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::Psar;
use vike_indicators::strategy_kit::parabolic_sar;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

const START: f64 = 0.02;
const STEP: f64 = 0.02;
const CAP: f64 = 0.2;

struct S {
    sar: Psar,
    was_below: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S { sar: parabolic_sar(START, STEP, CAP), was_below: false }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let below = self.sar.next(bar) < bar.close;
        let signal = match (self.was_below, below) {
            (false, true) => Signal::Long,
            (true, false) => Signal::Short,
            _ => Signal::Hold,
        };
        self.was_below = below;
        signal
    }
}
