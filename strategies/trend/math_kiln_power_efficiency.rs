// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/math-kiln-power-efficiency-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Efficiency is the 18-bar move of the close over 18 ATR(18), squared. Long 2 when flat and the close crosses up through EMA(43)
//! with efficiency above 0.035. Closed when it crosses down, efficiency falls below 0.035 x 0.35, or the close is more than 2.7
//! ATR under the average cost. 0.05% commission, 1 tick.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use std::collections::VecDeque;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;

const NAME: &str = "long";
const SIZE: f64 = 2.0;
const WINDOW: usize = 18;
const LEAST_EFFICIENCY: f64 = 0.035;
const FADED_SHARE: f64 = 0.35;
const MOST_ATRS_AGAINST: f64 = 2.7;

struct S {
    trend: IndicatorLine,
    atr: IndicatorLine,
    cross: Cross,
    /// The last `WINDOW` closes, oldest first.
    closes: VecDeque<f64>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        trend: IndicatorLine::new("ema", &[43.0]),
        atr: IndicatorLine::new("atr", &[18.0]),
        cross: Cross::new(),
        closes: VecDeque::with_capacity(WINDOW + 1),
    })
}

impl S {
    /// The path is at least one `tick`.
    fn efficiency(&mut self, close: f64, atr: f64, tick: f64) -> f64 {
        let back = if self.closes.len() == WINDOW { self.closes[0] } else { f64::NAN };
        self.closes.push_back(close);
        if self.closes.len() > WINDOW {
            self.closes.pop_front();
        }
        let path = (atr * WINDOW as f64).max(tick);
        let share = (close - back).abs() / path;
        share * share
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let trend = self.trend.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        let efficiency = self.efficiency(bar.close, atr, tick);
        let crossed = self.cross.step(bar.close, trend);
        let held = strategy.position();
        let ended = matches!(crossed, Signal::Short) || efficiency < LEAST_EFFICIENCY * FADED_SHARE;
        let too_far = held > 0.0 && bar.close < strategy.avg_cost() - atr * MOST_ATRS_AGAINST;
        if held == 0.0 && matches!(crossed, Signal::Long) && efficiency > LEAST_EFFICIENCY {
            strategy.entry(NAME, Long).size(SIZE);
        } else if held > 0.0 && (ended || too_far) {
            strategy.close(NAME);
        }
    }
}
