// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/bracket-atr-trail-series-int-points-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! When flat, long 1 at the open after 08:00 UTC and short 1 at the open after 20:00. Each carries, from the same pass, a stop 1.2
//! ATR(14) from the signal bar's close, a target twice that far on the other side, and an exit past the entry by ATR(14) read as a
//! count of ticks with no offset. A fractional count rounds up.
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
const STOP_ATRS: f64 = 1.2;
const TARGET_PER_STOP: f64 = 2.0;

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
        let away = atr * STOP_ATRS;
        let trail = TacticsTrail { after_ticks: atr.ceil(), by_ticks: 0.0 };
        if now == (8, 0) && strategy.position() == 0.0 {
            strategy.entry(UP, Long).size(1.0);
            strategy
                .exit("long exit")
                .from(UP)
                .take(bar.close + away * TARGET_PER_STOP)
                .stop(bar.close - away)
                .trail(trail);
        }
        if now == (20, 0) && strategy.position() == 0.0 {
            strategy.entry(DOWN, Short).size(1.0);
            strategy
                .exit("short exit")
                .from(DOWN)
                .take(bar.close - away * TARGET_PER_STOP)
                .stop(bar.close + away)
                .trail(trail);
        }
    }
}
