// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge contributors
// The script states no licence of its own: the source repository's LICENSE and LEGAL.md put it under Apache-2.0.
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/drawing-line-level-breakout/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when the close crosses up through a level, flat when it crosses down. The level starts at the first SMA(20) value and
//! then moves 35% of the way to SMA(20) on each bar. It never shorts.
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

const STEP: f64 = 0.35;

struct S {
    average: IndicatorLine,
    // The drawn line's right end: the only part of the line the rule reads back.
    level: f64,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        average: IndicatorLine::new("sma", &[20.0]),
        level: f64::NAN,
        cross: Cross::new(),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let average = self.average.next(bar)[0];
        if !average.is_nan() {
            self.level = if self.level.is_nan() {
                average
            } else {
                self.level + (average - self.level) * STEP
            };
        }
        match self.cross.step(bar.close, self.level) {
            Signal::Short => Signal::Close,
            other => other,
        }
    }
}
