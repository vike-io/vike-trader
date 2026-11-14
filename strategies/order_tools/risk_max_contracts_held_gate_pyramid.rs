// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/risk-max-contracts-held-gate-pyramid-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Adds 1 long at the open after every whole UTC hour, up to ten lots, until the largest position ever held has reached 5, and
//! never again after that, flat or not. The whole position closes at the open after 23:45.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::calendar::clock;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "long";
const MOST_LOTS: usize = 10;
const PEAK_LIMIT: f64 = 5.0;

struct S {
    peak: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S { peak: 0.0 })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { pyramiding: MOST_LOTS, ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        self.peak = self.peak.max(strategy.position().abs());
        let now = clock(bar);
        if now.1 == 0 && self.peak < PEAK_LIMIT {
            strategy.entry(NAME, Long).size(1.0);
        }
        if now == (23, 45) && strategy.position() != 0.0 {
            strategy.close_all();
        }
    }
}
