// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/cap-risk-gates-allow-max-intraday-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Longs of 1 at the opens after 00:15, 00:30 and 00:45 UTC, the third refused because the position may not pass 2. At the open after
//! 01:00 the rule asks for a short, which is not allowed, and all the asking does is close the long. Anything held
//! closes at the open after 03:00. Three fills a day; the close is the third, so it stops nothing the rule still wants.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! fills_a_day = 3
//! ```

use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::calendar::clock;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const MOST_LOTS: usize = 2;

struct S;

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S)
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { pyramiding: MOST_LOTS, ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        match clock(bar) {
            (0, 15) => {
                strategy.entry("first", Long).size(1.0);
            }
            (0, 30) => {
                strategy.entry("second", Long).size(1.0);
            }
            (0, 45) => {
                strategy.entry("third", Long).size(1.0);
            }
            (1, 0) | (3, 0) => {
                strategy.close_all();
            }
            _ => {}
        }
    }
}
