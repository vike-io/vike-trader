// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/bracket-trail-points-no-offset-explicit-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! When flat, long 1 at the open after 08:00 UTC and short 1 at the open after 20:00. Each carries, from the same pass, a stop and a
//! target 5% from the signal bar's close and an exit 8 ticks past the entry with no offset.
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
const WIDTH: f64 = 0.05;
const TRAIL: TacticsTrail = TacticsTrail { after_ticks: 8.0, by_ticks: 0.0 };

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
        let (above, below) = (bar.close * (1.0 + WIDTH), bar.close * (1.0 - WIDTH));
        if now == (8, 0) && strategy.position() == 0.0 {
            strategy.entry(UP, Long).size(1.0);
            strategy.exit("long exit").from(UP).take(above).stop(below).trail(TRAIL);
        }
        if now == (20, 0) && strategy.position() == 0.0 {
            strategy.entry(DOWN, Short).size(1.0);
            strategy.exit("short exit").from(DOWN).take(below).stop(above).trail(TRAIL);
        }
    }
}
