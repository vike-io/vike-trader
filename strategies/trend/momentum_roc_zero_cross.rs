// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-momentum-roc-zero-cross-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when 10-bar momentum crosses up through zero with the 10-bar rate of change above zero, short when it crosses down
//! with the rate of change below zero. A long closes once momentum is below zero, a short once it is above.
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
    momentum: IndicatorLine,
    rate: IndicatorLine,
    zero: Cross,
    // The position these rules asked for: a signal sees no broker.
    held: i8,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        momentum: IndicatorLine::new("mom", &[10.0]),
        rate: IndicatorLine::new("roc", &[10.0]),
        zero: Cross::new(),
        held: 0,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let momentum = self.momentum.next(bar)[0];
        let rate = self.rate.next(bar)[0];
        let crossed = self.zero.step(momentum, 0.0);

        let long = matches!(crossed, Signal::Long) && rate > 0.0;
        let short = matches!(crossed, Signal::Short) && rate < 0.0;
        let faded = (self.held > 0 && momentum < 0.0) || (self.held < 0 && momentum > 0.0);

        if long {
            self.held = 1;
            Signal::Long
        } else if short {
            self.held = -1;
            Signal::Short
        } else if faded {
            self.held = 0;
            Signal::Close
        } else {
            Signal::Hold
        }
    }
}
