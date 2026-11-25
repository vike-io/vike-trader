// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-range-filter-var-band-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! A level trails the close at a distance of 2.5 times the EMA(50) of the bar range. A close beyond that distance from the
//! level puts it the same distance behind the close and sets the direction; otherwise it only tightens toward the close in
//! the direction already set. Long when the direction turns up, short when it turns down.
//!
//! The level is written here because vike has no such indicator.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::Ema;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

const MULTIPLE: f64 = 2.5;

struct S {
    range_average: Ema,
    // Starts at the first close, and a band still warming up is NaN, which no comparison below passes.
    level: f64,
    direction: i8,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        range_average: Ema::new(50.0).expect("a valid period"),
        level: f64::NAN,
        direction: 0,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let band = self.range_average.next_value(bar.high - bar.low) * MULTIPLE;
        if self.level.is_nan() {
            self.level = bar.close;
        }
        let before = self.direction;

        if bar.close > self.level + band {
            self.level = bar.close - band;
            self.direction = 1;
        } else if bar.close < self.level - band {
            self.level = bar.close + band;
            self.direction = -1;
        } else if self.direction == 1 {
            self.level = self.level.max(bar.close - band);
        } else if self.direction == -1 {
            self.level = self.level.min(bar.close + band);
        }

        match self.direction {
            1 if before != 1 => Signal::Long,
            -1 if before != -1 => Signal::Short,
            _ => Signal::Hold,
        }
    }
}
