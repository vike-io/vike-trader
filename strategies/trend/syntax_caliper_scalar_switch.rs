// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/syntax-caliper-scalar-switch-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 5% of the account's worth when EMA(39) crosses up through EMA(113), short 5% when it crosses down, reversing what is held.
//! While held, a stop 2 ATR(15) from the average cost. 0.05% commission, 1 tick.
//!
//! The rule lets its filter be chosen by name; this is its default choice, the EMA. Its length is 29 x 1.35, rounded.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const UP: &str = "long";
const DOWN: &str = "short";
const SHARE: f64 = 0.05;
const BASE_LENGTH: f64 = 29.0;
const LENGTH_SCALE: f64 = 1.35;
const STOP_ATRS: f64 = 2.0;

struct S {
    filter: IndicatorLine,
    confirmation: IndicatorLine,
    atr: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        filter: IndicatorLine::new("ema", &[(BASE_LENGTH * LENGTH_SCALE).round()]),
        confirmation: IndicatorLine::new("ema", &[113.0]),
        atr: IndicatorLine::new("atr", &[15.0]),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let filter = self.filter.next(bar)[0];
        let confirmation = self.confirmation.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        match self.cross.step(filter, confirmation) {
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
        let distance = atr * STOP_ATRS;
        if held > 0.0 {
            let cut = strategy.avg_cost() - distance;
            strategy.exit("long guard").from(UP).stop(cut);
        }
        if held < 0.0 {
            let cut = strategy.avg_cost() + distance;
            strategy.exit("short guard").from(DOWN).stop(cut);
        }
    }
}
