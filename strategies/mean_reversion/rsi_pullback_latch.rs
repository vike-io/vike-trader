// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-4emarsi-rsi-pullback-latch-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when RSI(14) crosses up through 50 having been below 40 since it last did; short when it crosses down through 50
//! having been above 60 since it last did.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{CrossBands, Signal, SignalRule, SignalSource};

const MIDLINE: f64 = 50.0;
const BELOW_ARMS_LONG: f64 = 40.0;
const ABOVE_ARMS_SHORT: f64 = 60.0;

struct S {
    rsi: IndicatorLine,
    midline: CrossBands,
    long_ready: bool,
    short_ready: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        rsi: IndicatorLine::new("rsi", &[14.0]),
        midline: CrossBands::new(MIDLINE, MIDLINE),
        long_ready: false,
        short_ready: false,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let rsi = self.rsi.next(bar)[0];
        self.long_ready |= rsi < BELOW_ARMS_LONG;
        self.short_ready |= rsi > ABOVE_ARMS_SHORT;
        match self.midline.step(rsi) {
            Signal::Long if self.long_ready => {
                self.long_ready = false;
                Signal::Long
            }
            Signal::Short if self.short_ready => {
                self.short_ready = false;
                Signal::Short
            }
            _ => Signal::Hold,
        }
    }
}
