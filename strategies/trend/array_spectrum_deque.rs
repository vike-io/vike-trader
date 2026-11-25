// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/array-spectrum-deque-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! The last 17 one-bar returns: the share that are positive, and their average. Long 2 when flat, the share has just reached 0.64,
//! the average is above 0 and the close is above EMA(49). Closed when the share is under 0.46, the average is under 0, the close is
//! under the EMA, or the close is more than 2.4 ATR(17) under the average cost. 0.05% commission, 1 tick.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use std::collections::VecDeque;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_tactics::{Rule, RuleDefaults, Tactics};

use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::Long;
use vike_model::params::Params;

const NAME: &str = "long";
const SIZE: f64 = 2.0;
const SAMPLES: usize = 17;
const ENTER_SHARE: f64 = 0.64;
const LEAVE_SHARE: f64 = 0.46;
const MOST_ATRS_AGAINST: f64 = 2.4;

struct S {
    one_bar: IndicatorLine,
    returns: VecDeque<f64>,
    last_share: f64,
    trend: IndicatorLine,
    atr: IndicatorLine,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        one_bar: IndicatorLine::new("roc", &[1.0]),
        returns: VecDeque::with_capacity(SAMPLES + 1),
        last_share: f64::NAN,
        trend: IndicatorLine::new("ema", &[49.0]),
        atr: IndicatorLine::new("atr", &[17.0]),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        self.returns.push_back(self.one_bar.next(bar)[0]);
        if self.returns.len() > SAMPLES {
            self.returns.pop_front();
        }
        let count = self.returns.len() as f64;
        let positive = self.returns.iter().filter(|&&r| r > 0.0).count() as f64;
        let share = positive / count;
        let average = self.returns.iter().sum::<f64>() / count;
        let was_below = self.last_share < ENTER_SHARE;
        self.last_share = share;

        let trend = self.trend.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        let enter = share >= ENTER_SHARE && average > 0.0 && bar.close > trend && was_below;
        let ended = share < LEAVE_SHARE || average < 0.0 || bar.close < trend;
        let held = strategy.position();
        let too_far = held > 0.0 && bar.close < strategy.avg_cost() - atr * MOST_ATRS_AGAINST;
        if held == 0.0 && enter {
            strategy.entry(NAME, Long).size(SIZE);
        } else if held > 0.0 && (ended || too_far) {
            strategy.close(NAME);
        }
    }
}
