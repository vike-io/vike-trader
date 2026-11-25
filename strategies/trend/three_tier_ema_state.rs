// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-trendmaster-three-tier-ema-state-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long on the bar EMA(21) > EMA(55) > EMA(200) becomes true, short on the bar the opposite order becomes true.
//!
//! The averages start cold.
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

struct S {
    averages: [Ema; 3],
    // 1 in bullish order, -1 in bearish order, 0 otherwise.
    last_order: i32,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        averages: [21.0, 55.0, 200.0].map(|n| Ema::new(n).expect("a valid period")),
        last_order: 0,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let [a, b, c] = [0, 1, 2].map(|i| self.averages[i].next(bar));
        let order = if a > b && b > c {
            1
        } else if a < b && b < c {
            -1
        } else {
            0
        };
        let changed = order != self.last_order;
        self.last_order = order;
        match order {
            1 if changed => Signal::Long,
            -1 if changed => Signal::Short,
            _ => Signal::Hold,
        }
    }
}
