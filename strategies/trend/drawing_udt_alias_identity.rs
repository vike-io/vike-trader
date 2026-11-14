// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge contributors
// The script states no licence of its own: the source repository's LICENSE and LEGAL.md put it under Apache-2.0.
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/drawing-udt-alias-identity/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when the close crosses up through SMA(18), flat when it crosses down. Until the average exists the level is the close
//! itself, so the first real average can already be a cross. It never shorts.
//!
//! The level is written to a drawn line through one reference and read back through another, and both name the same line, so what
//! comes back is that bar's average.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Cross, Signal, SignalRule, SignalSource};

struct S {
    average: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S { average: IndicatorLine::new("sma", &[18.0]), cross: Cross::new() }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let average = self.average.next(bar)[0];
        let level = if average.is_nan() { bar.close } else { average };
        match self.cross.step(bar.close, level) {
            Signal::Short => Signal::Close,
            other => other,
        }
    }
}
