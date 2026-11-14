// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-cross-entry-close-same-pass-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Four daily windows of size 1, each issuing an entry and a close of its own name in one pass, in either order; the close of a
//! name not yet held does nothing. Long after 00:15 UTC or 06:15 when flat, flattened after 07:15. Short after 12:15 or 18:15
//! when flat, flattened after 19:15.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::calendar::clock;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const MORNING: &str = "morning";
const MORNING_LATE: &str = "morning late";
const EVENING: &str = "evening";
const EVENING_LATE: &str = "evening late";

struct S;

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S)
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let now = clock(bar);
        let held = strategy.position();
        if now == (0, 15) && held == 0.0 {
            strategy.entry(MORNING, Long).size(1.0);
            strategy.close(MORNING);
        }
        if now == (6, 15) && held == 0.0 {
            strategy.close(MORNING_LATE);
            strategy.entry(MORNING_LATE, Long).size(1.0);
        }
        if now == (7, 15) && held > 0.0 {
            strategy.close_all();
        }
        if now == (12, 15) && held == 0.0 {
            strategy.entry(EVENING, Short).size(1.0);
            strategy.close(EVENING);
        }
        if now == (18, 15) && held == 0.0 {
            strategy.close(EVENING_LATE);
            strategy.entry(EVENING_LATE, Short).size(1.0);
        }
        if now == (19, 15) && held < 0.0 {
            strategy.close_all();
        }
    }
}
