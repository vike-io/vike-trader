// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-ema-ribbon-stack-transition-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when EMA(8) is above EMA(21) and EMA(21) above EMA(55), short when they are stacked the other way. Each stack fires
//! once until the EMAs leave it: a bar in neither stack forgets the last one, so the same stack can fire again.
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

struct S {
    fast: IndicatorLine,
    mid: IndicatorLine,
    slow: IndicatorLine,
    /// The stack that last fired, +1 up and -1 down, 0 after a bar in neither.
    last_stack: i8,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        fast: IndicatorLine::new("ema", &[8.0]),
        mid: IndicatorLine::new("ema", &[21.0]),
        slow: IndicatorLine::new("ema", &[55.0]),
        last_stack: 0,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let fast = self.fast.next(bar)[0];
        let mid = self.mid.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        let up = fast > mid && mid > slow;
        let down = fast < mid && mid < slow;

        if up && self.last_stack != 1 {
            self.last_stack = 1;
            Signal::Long
        } else if down && self.last_stack != -1 {
            self.last_stack = -1;
            Signal::Short
        } else {
            if !up && !down {
                self.last_stack = 0;
            }
            Signal::Hold
        }
    }
}
