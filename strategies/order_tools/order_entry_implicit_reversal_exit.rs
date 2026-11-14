// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-entry-implicit-reversal-exit-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Each UTC day, by bar open: long at 00:15 when flat, reverse to short at 00:45 when long, reverse to long at 01:15 when short,
//! and flatten at 01:45 when anything is held.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

const DAY_MS: i64 = 86_400_000;
const MINUTE_MS: i64 = 60_000;

struct S {
    // The runner would open a short from flat at 00:45 or reverse at 00:15; these rules never do.
    held: i32,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S { held: 0 }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let minutes = bar.timestamp.rem_euclid(DAY_MS) / MINUTE_MS;
        let (signal, held) = match (minutes, self.held) {
            (15, 0) => (Signal::Long, 1),
            (45, 1) => (Signal::Short, -1),
            (75, -1) => (Signal::Long, 1),
            (105, 1 | -1) => (Signal::Close, 0),
            (_, held) => (Signal::Hold, held),
        };
        self.held = held;
        signal
    }
}
