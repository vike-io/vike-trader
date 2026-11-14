// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-kanuck-calc-on-every-tick-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when EMA(8) crosses up through EMA(21), short when it crosses down, but only on every third bar counted from the first bar
//! the rule saw. An opposite position reverses and the same side already held is ignored. It asks to recalculate on every tick,
//! which on historical bars means once per bar, so the counter moves once per bar.
//!
//! The count remembers where it started.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::Ema;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Cross, Signal, SignalRule, SignalSource};

const EVERY: u64 = 3;

struct S {
    fast: Ema,
    slow: Ema,
    cross: Cross,
    bars_seen: u64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        fast: Ema::new(8.0).expect("a valid period"),
        slow: Ema::new(21.0).expect("a valid period"),
        cross: Cross::new(),
        bars_seen: 0,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        self.bars_seen += 1;
        let crossed = self.cross.step(self.fast.next(bar), self.slow.next(bar));
        if self.bars_seen.is_multiple_of(EVERY) { crossed } else { Signal::Hold }
    }
}
