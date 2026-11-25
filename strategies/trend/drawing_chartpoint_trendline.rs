// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge contributors
// The script states no licence of its own: the source repository's LICENSE and LEGAL.md put it under Apache-2.0.
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/drawing-chartpoint-trendline/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Short when the close crosses down through a level, flat when it crosses up. The level is a line's price one bar back, on a line
//! from SMA(20) two bars ago to SMA(20) now, which is halfway between them. Until both exist the level is the close itself, so
//! the first real level can already be a cross. It never goes long.
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
    // The last two averages, oldest first, NaN until they exist.
    previous: [f64; 2],
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        average: IndicatorLine::new("sma", &[20.0]),
        previous: [f64::NAN; 2],
        cross: Cross::new(),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let average = self.average.next(bar)[0];
        let two_back = self.previous[0];
        self.previous = [self.previous[1], average];

        let on_line = two_back + (average - two_back) / 2.0;
        let level = if on_line.is_nan() { bar.close } else { on_line };
        match self.cross.step(bar.close, level) {
            Signal::Long => Signal::Close,
            other => other,
        }
    }
}
