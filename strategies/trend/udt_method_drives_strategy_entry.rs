// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/udt-method-drives-strategy-entry-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when EMA(9) crosses up through EMA(21), short when it crosses down, each reversing the other. A cross counts only if at
//! least 5 bars passed since the last counted cross in the same direction, and an uncounted one does nothing.
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

const NEVER: i64 = -1000;

struct Spacer {
    last_bar: i64,
    min_gap: i64,
}

impl Spacer {
    fn allows(&mut self, bar_number: i64, crossed: bool) -> bool {
        let allowed = crossed && bar_number - self.last_bar >= self.min_gap;
        if allowed {
            self.last_bar = bar_number;
        }
        allowed
    }
}

struct S {
    bar_number: i64,
    long_spacer: Spacer,
    short_spacer: Spacer,
    fast: IndicatorLine,
    slow: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        bar_number: 0,
        long_spacer: Spacer { last_bar: NEVER, min_gap: 5 },
        short_spacer: Spacer { last_bar: NEVER, min_gap: 5 },
        fast: IndicatorLine::new("ema", &[9.0]),
        slow: IndicatorLine::new("ema", &[21.0]),
        cross: Cross::new(),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        let crossed = self.cross.step(fast, slow);
        let now = self.bar_number;
        self.bar_number += 1;
        if self.long_spacer.allows(now, matches!(crossed, Signal::Long)) {
            Signal::Long
        } else if self.short_spacer.allows(now, matches!(crossed, Signal::Short)) {
            Signal::Short
        } else {
            Signal::Hold
        }
    }
}
