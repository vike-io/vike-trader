// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/analyzer-cashflow-cash-costs-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long a fixed 4200 of value when flat and EMA(22) crosses up through EMA(63). Closed when EMA(22) crosses down, or the close
//! is more than 2.6 ATR(18) under the average cost. Costs: 0.35 per contract on each fill, 1 tick. Starts with 75,000.
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
const BUDGET: f64 = 4200.0;
const MOST_ATRS_AGAINST: f64 = 2.6;

struct S {
    fast: IndicatorLine,
    slow: IndicatorLine,
    atr: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: IndicatorLine::new("ema", &[22.0]),
        slow: IndicatorLine::new("ema", &[63.0]),
        atr: IndicatorLine::new("atr", &[18.0]),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::per_contract(0.35), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        let cross = self.cross.step(fast, slow);
        let held = strategy.position();
        let too_far = held > 0.0 && bar.close < strategy.avg_cost() - atr * MOST_ATRS_AGAINST;
        if held == 0.0 && matches!(cross, Signal::Long) {
            let size = BUDGET / sizing.slipped(Long, bar.close);
            strategy.entry(NAME, Long).size(size);
        } else if held > 0.0 && (matches!(cross, Signal::Short) || too_far) {
            strategy.close(NAME);
        }
    }
}
