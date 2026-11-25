// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge contributors
// The script states no licence of its own: the source repository's LICENSE and LEGAL.md put it under Apache-2.0.
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/drawing-delete-halt/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when the close crosses up through SMA(20), flat when it crosses down. It never shorts.
//!
//! The original draws its level as a line, reads it back and deletes the line on the fifth entry. This rule reads the average
//! itself on every bar and draws nothing, so it has no line to delete.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::Sma;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Cross, Signal, SignalRule, SignalSource};

/// A close level with the average, within this distance of it, counts as below it. The average is summed afresh over its
/// window (`with_fresh_sum`): vike's running sum can land a hair under a close it equals.
const TIE: f64 = 1e-9;

struct S {
    level: Sma,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        level: Sma::new(20.0).expect("a valid period").with_fresh_sum(true),
        cross: Cross::new(),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let level = self.level.next(bar);
        match self.cross.step(bar.close, level + TIE) {
            Signal::Short => Signal::Close,
            other => other,
        }
    }
}
