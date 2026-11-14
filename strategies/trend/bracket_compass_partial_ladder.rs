// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/bracket-compass-partial-ladder-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 5 when EMA(15) crosses up through EMA(49). While held, two exits around the average cost on every bar, sharing a stop 1.9
//! ATR(17) under it: 2 lots with a target 1.4 ATR(17) above it, 3 lots with a target 3 ATR(17) above it. One that fires stays spent
//! and leaves the other working at its own size. 0.05% commission, 1 tick, 100,000 of cash.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const NAME: &str = "long";
const LOT: f64 = 5.0;
const FIRST_SHARE: f64 = 0.4;
const STOP_ATRS: f64 = 1.9;
const FIRST_TARGET_ATRS: f64 = 1.4;
const SECOND_TARGET_ATRS: f64 = 3.0;

struct S {
    fast: IndicatorLine,
    slow: IndicatorLine,
    atr: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: IndicatorLine::new("ema", &[15.0]),
        slow: IndicatorLine::new("ema", &[49.0]),
        atr: IndicatorLine::new("atr", &[17.0]),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        if matches!(self.cross.step(fast, slow), Signal::Long) {
            strategy.entry(NAME, Long).size(LOT);
        }
        if strategy.position() > 0.0 {
            let cost = strategy.avg_cost();
            let cut = cost - atr * STOP_ATRS;
            strategy
                .exit("first")
                .from(NAME)
                .size(LOT * FIRST_SHARE)
                .take(cost + atr * FIRST_TARGET_ATRS)
                .stop(cut);
            strategy
                .exit("second")
                .from(NAME)
                .size(LOT * (1.0 - FIRST_SHARE))
                .take(cost + atr * SECOND_TARGET_ATRS)
                .stop(cut);
        }
    }
}
