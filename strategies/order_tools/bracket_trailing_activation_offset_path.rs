// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/bracket-trailing-activation-offset-path-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! When flat, long 1 at the open after 09:15 UTC and short 1 at the open after 15:15. While held, a trailing stop that waits
//! until the price is 800 ticks past the entry and then follows the best price by 400 ticks. Flatten at the open after 23:45.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::calendar::clock;
use vike_tactics::{Rule, RuleDefaults, Tactics, TacticsTrail};

const UP: &str = "long";
const DOWN: &str = "short";
const TRAIL: TacticsTrail = TacticsTrail { after_ticks: 800.0, by_ticks: 400.0 };

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
        if now == (9, 15) && held == 0.0 {
            strategy.entry(UP, Long).size(1.0);
        }
        if now == (15, 15) && held == 0.0 {
            strategy.entry(DOWN, Short).size(1.0);
        }
        if held > 0.0 {
            strategy.exit("long trail").from(UP).trail(TRAIL);
        }
        if held < 0.0 {
            strategy.exit("short trail").from(DOWN).trail(TRAIL);
        }
        if held != 0.0 && now == (23, 45) {
            strategy.close_all();
        }
    }
}
