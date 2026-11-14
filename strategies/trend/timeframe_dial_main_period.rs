// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/timeframe-dial-main-period-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 2 when flat and the fast EMA crosses up through the slow one, closed when it crosses down or the close is more than 2.4
//! ATR(15) under the average cost. The rule picks the pair by chart period; only the 15-minute pair, EMA(20) and EMA(44), is
//! written here. 0.05% commission, 1 tick.
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
const MOST_ATRS_AGAINST: f64 = 2.4;

struct S {
    fast: IndicatorLine,
    slow: IndicatorLine,
    atr: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: IndicatorLine::new("ema", &[20.0]),
        slow: IndicatorLine::new("ema", &[44.0]),
        atr: IndicatorLine::new("atr", &[15.0]),
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
        let crossed = self.cross.step(fast, slow);
        let held = strategy.position();
        let too_far = held > 0.0 && bar.close < strategy.avg_cost() - atr * MOST_ATRS_AGAINST;
        if held == 0.0 && matches!(crossed, Signal::Long) {
            strategy.entry(NAME, Long).size(SIZE);
        } else if held > 0.0 && (matches!(crossed, Signal::Short) || too_far) {
            strategy.close(NAME);
        }
    }
}
