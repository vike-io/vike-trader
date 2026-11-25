// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/enum-relay-regime-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 2 when EMA(16) crosses up through EMA(45), closed when it crosses down. While held, a stop 2 ATR(16) under the average
//! cost. 0.05% commission, 1 tick.
//!
//! The rule also has a mean-reversion regime on RSI, chosen by an enum; this is its default, the trend regime.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "long";
const SIZE: f64 = 2.0;
const STOP_ATRS: f64 = 2.0;

struct S {
    fast: IndicatorLine,
    slow: IndicatorLine,
    atr: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: IndicatorLine::new("ema", &[16.0]),
        slow: IndicatorLine::new("ema", &[45.0]),
        atr: IndicatorLine::new("atr", &[16.0]),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        let crossed = self.cross.step(fast, slow);
        if matches!(crossed, Signal::Long) {
            strategy.entry(NAME, Long).size(SIZE);
        }
        let held = strategy.position() > 0.0;
        if held && matches!(crossed, Signal::Short) {
            strategy.close(NAME);
        }
        if held {
            let cut = strategy.avg_cost() - atr * STOP_ATRS;
            strategy.exit("guard").from(NAME).stop(cut);
        }
    }
}
