// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/bracket-aurora-market-atr-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when EMA(18) crosses up through EMA(47), short when it crosses down, reversing each other, each worth a tenth of the account
//! at this close. While held, on every bar, a stop 1.8 ATR(16) and a target 2.7 ATR(16) from the average cost. 0.05% commission, 1
//! tick, 100,000 of cash.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const UP: &str = "long";
const DOWN: &str = "short";
const SHARE: f64 = 0.1;
const STOP_ATRS: f64 = 1.8;
const TARGET_ATRS: f64 = 2.7;

struct S {
    fast: IndicatorLine,
    slow: IndicatorLine,
    atr: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: IndicatorLine::new("ema", &[18.0]),
        slow: IndicatorLine::new("ema", &[47.0]),
        atr: IndicatorLine::new("atr", &[16.0]),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        match self.cross.step(fast, slow) {
            Signal::Long => {
                let size = sizing.size_for_share(SHARE, sizing.slipped(Long, bar.close));
                strategy.entry(UP, Long).size(size);
            }
            Signal::Short => {
                let size = sizing.size_for_share(SHARE, sizing.slipped(Short, bar.close));
                strategy.entry(DOWN, Short).size(size);
            }
            _ => {}
        }
        let held = strategy.position();
        let cost = strategy.avg_cost();
        if held > 0.0 {
            strategy
                .exit("long bracket")
                .from(UP)
                .take(cost + atr * TARGET_ATRS)
                .stop(cost - atr * STOP_ATRS);
        }
        if held < 0.0 {
            strategy
                .exit("short bracket")
                .from(DOWN)
                .take(cost - atr * TARGET_ATRS)
                .stop(cost + atr * STOP_ATRS);
        }
    }
}
