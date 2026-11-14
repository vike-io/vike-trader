// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/udt-breaker-method-while-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! A ladder of RSI(14) levels from 42 in steps of 4, at most 10 of them and none above 70: count the levels RSI(14) is above. Long 2
//! when flat, the count has just reached 4 and the close is above EMA(45). Closed when the count is under 2, the close is under the
//! EMA, or the close is more than 2.3 ATR(17) under the average cost. 0.05% commission, 1 tick.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const NAME: &str = "long";
const SIZE: f64 = 2.0;
const FIRST_LEVEL: f64 = 42.0;
const LEVEL_STEP: f64 = 4.0;
const TOP_LEVEL: f64 = 70.0;
const MOST_LEVELS: usize = 10;
const ENTER_COUNT: usize = 4;
const MOST_ATRS_AGAINST: f64 = 2.3;

/// How many rungs the value clears, stopping at the first rung above the top.
fn levels_cleared(value: f64) -> usize {
    (0..MOST_LEVELS)
        .map(|rung| FIRST_LEVEL + LEVEL_STEP * rung as f64)
        .take_while(|&level| level <= TOP_LEVEL)
        .filter(|&level| value > level)
        .count()
}

struct S {
    rsi: IndicatorLine,
    trend: IndicatorLine,
    atr: IndicatorLine,
    was_ready: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    assert_eq!((levels_cleared(f64::NAN), levels_cleared(54.5), levels_cleared(99.0)), (0, 4, 8));
    Box::new(S {
        rsi: IndicatorLine::new("rsi", &[14.0]),
        trend: IndicatorLine::new("ema", &[45.0]),
        atr: IndicatorLine::new("atr", &[17.0]),
        was_ready: false,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let cleared = levels_cleared(self.rsi.next(bar)[0]);
        let trend = self.trend.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        let ready = cleared >= ENTER_COUNT;
        let enter = ready && !self.was_ready && bar.close > trend;
        self.was_ready = ready;
        let ended = cleared < ENTER_COUNT.saturating_sub(2).max(1) || bar.close < trend;
        let held = strategy.position();
        let too_far = held > 0.0 && bar.close < strategy.avg_cost() - atr * MOST_ATRS_AGAINST;
        if held == 0.0 && enter {
            strategy.entry(NAME, Long).size(SIZE);
        } else if held > 0.0 && (ended || too_far) {
            strategy.close(NAME);
        }
    }
}
