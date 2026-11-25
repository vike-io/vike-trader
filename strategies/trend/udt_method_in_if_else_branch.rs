// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/udt-method-in-if-else-branch-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long from the third bar in a row with EMA(9) more than 0.1% above EMA(21), flat from the third bar in a row with it more than
//! 0.1% below. A bar between those two resets both streaks. It never shorts.
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

struct Streaks {
    above: u32,
    below: u32,
}

impl Streaks {
    /// `side` is 1 above the band, -1 below it, anything else inside. Returns the streak above.
    fn record(&mut self, side: i32) -> u32 {
        if side == 1 {
            self.above += 1;
            self.below = 0;
        } else if side == -1 {
            self.below += 1;
            self.above = 0;
        } else {
            self.above = 0;
            self.below = 0;
        }
        self.above
    }
}

struct S {
    streaks: Streaks,
    fast: IndicatorLine,
    slow: IndicatorLine,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        streaks: Streaks { above: 0, below: 0 },
        fast: IndicatorLine::new("ema", &[9.0]),
        slow: IndicatorLine::new("ema", &[21.0]),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        let above = if fast > slow * 1.001 {
            self.streaks.record(1)
        } else if fast < slow * 0.999 {
            self.streaks.record(-1)
        } else {
            self.streaks.record(0)
        };
        if above >= 3 {
            Signal::Long
        } else if self.streaks.below >= 3 {
            Signal::Close
        } else {
            Signal::Hold
        }
    }
}
