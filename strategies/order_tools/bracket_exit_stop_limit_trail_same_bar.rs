// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/bracket-exit-stop-limit-trail-same-bar-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! When flat, long 1 at the open after 08:15 UTC and short 1 at the open after 16:15. While held and on every bar: a stop 0.8 ATR(14)
//! and a target 1.6 ATR(14) from the average cost, and an exit past the entry by ATR(14) read as a count of ticks, rounded up, with
//! no offset. Flatten at the open after 23:45.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::calendar::clock;
use vike_tactics::{Rule, RuleDefaults, Tactics, TacticsTrail};

const UP: &str = "long";
const DOWN: &str = "short";
const STOP_ATRS: f64 = 0.8;
const TARGET_ATRS: f64 = 1.6;

struct S {
    atr: IndicatorLine,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S { atr: IndicatorLine::new("atr", &[14.0]) })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let atr = self.atr.next(bar)[0];
        let now = clock(bar);
        let trail = TacticsTrail { after_ticks: atr.ceil(), by_ticks: 0.0 };
        if now == (8, 15) && strategy.position() == 0.0 {
            strategy.entry(UP, Long).size(1.0);
        }
        if now == (16, 15) && strategy.position() == 0.0 {
            strategy.entry(DOWN, Short).size(1.0);
        }
        let cost = strategy.avg_cost();
        if strategy.position() > 0.0 {
            strategy
                .exit("long exit")
                .from(UP)
                .take(cost + atr * TARGET_ATRS)
                .stop(cost - atr * STOP_ATRS)
                .trail(trail);
        }
        if strategy.position() < 0.0 {
            strategy
                .exit("short exit")
                .from(DOWN)
                .take(cost - atr * TARGET_ATRS)
                .stop(cost + atr * STOP_ATRS)
                .trail(trail);
        }
        if strategy.position() != 0.0 && now == (23, 45) {
            strategy.close_all();
        }
    }
}
