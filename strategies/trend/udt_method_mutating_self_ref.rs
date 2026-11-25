// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/udt-method-mutating-self-ref-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long on the third green bar in a row or any later one, flat on the first bar that is not green. A green bar closes above its
//! open. It never shorts.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

struct Streak {
    length: u32,
}

impl Streak {
    fn advance(&mut self, keeps_going: bool) -> u32 {
        self.length = if keeps_going { self.length + 1 } else { 0 };
        self.length
    }
}

struct S {
    green_bars: Streak,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S { green_bars: Streak { length: 0 } }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let green = bar.close > bar.open;
        let length = self.green_bars.advance(green);
        if length >= 3 {
            Signal::Long
        } else if !green {
            Signal::Close
        } else {
            Signal::Hold
        }
    }
}
