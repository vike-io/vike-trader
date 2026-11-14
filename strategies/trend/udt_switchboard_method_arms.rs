// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/udt-switchboard-method-arms-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! A score: EMA(15) minus EMA(47), over ATR(18) with one tick as the floor, times a sensitivity. Long 2 when flat and the score
//! crosses up through 0.32, closed when it is under 0 or the close is more than 2.3 ATR(18) under the average cost. The rule picks
//! the sensitivity by a mode, 1.55, 1 or 0.68, and its default middle mode gives 1. 0.05% commission, 1 tick.
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
const SIZE: f64 = 2.0;
const SENSITIVITY: f64 = 1.0;
const ENTER_ABOVE: f64 = 0.32;
const MOST_ATRS_AGAINST: f64 = 2.3;

struct S {
    fast: IndicatorLine,
    slow: IndicatorLine,
    atr: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: IndicatorLine::new("ema", &[15.0]),
        slow: IndicatorLine::new("ema", &[47.0]),
        atr: IndicatorLine::new("atr", &[18.0]),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        let score = (fast - slow) / atr.max(tick) * SENSITIVITY;
        let enter = matches!(self.cross.step(score, ENTER_ABOVE), Signal::Long);
        let held = strategy.position();
        let too_far = held > 0.0 && bar.close < strategy.avg_cost() - atr * MOST_ATRS_AGAINST;
        if held == 0.0 && enter {
            strategy.entry(NAME, Long).size(SIZE);
        } else if held > 0.0 && (score < 0.0 || too_far) {
            strategy.close(NAME);
        }
    }
}
