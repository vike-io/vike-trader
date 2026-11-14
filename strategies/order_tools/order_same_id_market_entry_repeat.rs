// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-same-id-market-entry-repeat-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! When flat, long 1 at the open after 00:15 UTC and short 1 at the open after 12:15, each asked for twice in one pass under
//! one name, which stays one order though three lots are allowed. Closed at the open after 01:15 and after 13:15.
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

const UP: &str = "long";
const DOWN: &str = "short";
const MOST_LOTS: usize = 3;

struct S;

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S)
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { pyramiding: MOST_LOTS, ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let now = clock(bar);
        let held = strategy.position();
        if now == (0, 15) && held == 0.0 {
            strategy.entry(UP, Long).size(1.0);
            strategy.entry(UP, Long).size(1.0);
        }
        if now == (1, 15) && held > 0.0 {
            strategy.close(UP);
        }
        if now == (12, 15) && held == 0.0 {
            strategy.entry(DOWN, Short).size(1.0);
            strategy.entry(DOWN, Short).size(1.0);
        }
        if now == (13, 15) && held < 0.0 {
            strategy.close(DOWN);
        }
    }
}
