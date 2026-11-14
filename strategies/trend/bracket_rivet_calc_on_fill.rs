// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/bracket-rivet-calc-on-fill-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 2, filling at this close, when flat and EMA(11) crosses up through EMA(37). While held, a stop 1.7 ATR(15) under and a target
//! 2.2 ATR(15) above the average cost. The rule runs at each bar's close and again after each fill inside a bar, reading that bar's
//! final values, so a stop that fires on a bar whose close makes the cross opens the long at the next point of the walk. 0.05%
//! commission, 1 tick, 100,000 of cash.
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
const STOP_ATRS: f64 = 1.7;
const TARGET_ATRS: f64 = 2.2;

struct S {
    fast: IndicatorLine,
    slow: IndicatorLine,
    atr: IndicatorLine,
    cross: Cross,
    /// The bar the indicators have already seen: a fill inside a bar feeds them ahead of its close.
    fed: Option<i64>,
    crossed_up: bool,
    atr_now: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: IndicatorLine::new("ema", &[11.0]),
        slow: IndicatorLine::new("ema", &[37.0]),
        atr: IndicatorLine::new("atr", &[15.0]),
        cross: Cross::new(),
        fed: None,
        crossed_up: false,
        atr_now: f64::NAN,
    })
}

impl S {
    fn run_rule(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        if self.fed != Some(bar.timestamp) {
            self.fed = Some(bar.timestamp);
            let fast = self.fast.next(bar)[0];
            let slow = self.slow.next(bar)[0];
            self.atr_now = self.atr.next(bar)[0];
            self.crossed_up = matches!(self.cross.step(fast, slow), Signal::Long);
        }
        if self.crossed_up && strategy.position() == 0.0 {
            strategy.entry(NAME, Long).size(2.0);
        }
        if strategy.position() > 0.0 {
            let cost = strategy.avg_cost();
            strategy
                .exit("bracket")
                .from(NAME)
                .take(cost + self.atr_now * TARGET_ATRS)
                .stop(cost - self.atr_now * STOP_ATRS);
        }
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        self.run_rule(strategy, bar);
    }

    fn after_fill(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        self.run_rule(strategy, bar);
    }
}
