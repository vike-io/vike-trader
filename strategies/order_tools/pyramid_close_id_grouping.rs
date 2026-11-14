// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/pyramid-close-id-grouping-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Three longs of 1 under one name, at the opens after 00:15, 00:30 and 00:45 UTC, the first only when flat, the others only when
//! long, all closed by that name at the open after 01:15. The same three shorts twelve hours later: 12:15, 12:30, 12:45, closed
//! after 13:15. No costs.
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

const LONG_LOTS: &str = "long";
const SHORT_LOTS: &str = "short";
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
            strategy.entry(LONG_LOTS, Long).size(1.0);
        }
        if (now == (0, 30) || now == (0, 45)) && held > 0.0 {
            strategy.entry(LONG_LOTS, Long).size(1.0);
        }
        if now == (1, 15) && held > 0.0 {
            strategy.close(LONG_LOTS);
        }
        if now == (12, 15) && held == 0.0 {
            strategy.entry(SHORT_LOTS, Short).size(1.0);
        }
        if (now == (12, 30) || now == (12, 45)) && held < 0.0 {
            strategy.entry(SHORT_LOTS, Short).size(1.0);
        }
        if now == (13, 15) && held < 0.0 {
            strategy.close(SHORT_LOTS);
        }
    }
}
