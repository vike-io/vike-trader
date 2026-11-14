// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-market-close-fill-basis-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Each UTC day, by bar open: long at 03:15 when flat and close at 05:15 when long, short at 15:15 when flat and close at 17:15
//! when short. Every order fills at the next bar's open.
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
    // The runner would reverse from an opposite position at an entry time, or close either side at an exit time; these rules
    // never do.
    held: i32,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S { held: 0 }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let minutes = bar.timestamp.rem_euclid(DAY_MS) / MINUTE_MS;
        let (signal, held) = match (minutes / 60, minutes % 60, self.held) {
            (3, 15, 0) => (Signal::Long, 1),
            (5, 15, 1) => (Signal::Close, 0),
            (15, 15, 0) => (Signal::Short, -1),
            (17, 15, -1) => (Signal::Close, 0),
            (_, _, held) => (Signal::Hold, held),
        };
        self.held = held;
        signal
    }
}
