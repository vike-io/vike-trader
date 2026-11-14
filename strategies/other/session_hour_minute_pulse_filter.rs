// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/session-hour-minute-pulse-filter-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long on the bar opening at 00:15 UTC when flat, close on the bar opening at 00:45 UTC when long.
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

struct S;

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        match bar.timestamp.rem_euclid(DAY_MS) / MINUTE_MS {
            15 => Signal::Long,
            45 => Signal::Close,
            _ => Signal::Hold,
        }
    }
}
