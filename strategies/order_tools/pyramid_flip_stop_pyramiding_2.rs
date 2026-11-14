// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/pyramid-flip-stop-pyramiding-2-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Up to two lots a side, asked size 1. Flat on the 00:15 UTC bar, a market long; long on the 00:45 bar, a sell stop one tick under
//! that bar's low, then a close of that long. Flat on the 06:15 bar, a second market long; long on the 06:45 bar, a close of that
//! long, then a sell stop one tick under that bar's low. A short held on the 12:15 bar closes at the next open.
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
const MORNING_STOP: &str = "morning stop";
const NOON: &str = "noon";
const NOON_STOP: &str = "noon stop";
const LOTS_A_SIDE: usize = 2;

struct FlipStops;

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(FlipStops)
}

impl Rule for FlipStops {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { pyramiding: LOTS_A_SIDE, ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let now = clock(bar);
        let held = strategy.position();
        let under = bar.low - tick;
        if now == (0, 15) && held == 0.0 {
            strategy.entry(MORNING, Long).size(1.0);
        }
        if now == (0, 45) && held > 0.0 {
            strategy.entry(MORNING_STOP, Short).size(1.0).stop(under);
            strategy.close(MORNING);
        }
        if now == (6, 15) && held == 0.0 {
            strategy.entry(NOON, Long).size(1.0);
        }
        if now == (6, 45) && held > 0.0 {
            strategy.close(NOON);
            strategy.entry(NOON_STOP, Short).size(1.0).stop(under);
        }
        if now == (12, 15) && held < 0.0 {
            strategy.close_all();
        }
    }
}
