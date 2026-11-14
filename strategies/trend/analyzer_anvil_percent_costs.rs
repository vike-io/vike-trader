// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/analyzer-anvil-percent-costs-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 7.5% of the account's worth when SMA(23) crosses up through SMA(71), closed when it crosses down. While held, a stop
//! 3.4% below and a target 5.2% above the average cost. Costs: 0.08% commission, 2 ticks. Starts with 100,000.
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
const SHARE: f64 = 0.075;
const STOP: f64 = 0.034;
const TARGET: f64 = 0.052;

struct S {
    fast: IndicatorLine,
    slow: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: IndicatorLine::new("sma", &[23.0]),
        slow: IndicatorLine::new("sma", &[71.0]),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.08), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        match self.cross.step(fast, slow) {
            Signal::Long => {
                let size = sizing.size_for_share(SHARE, sizing.slipped(Long, bar.close));
                strategy.entry(NAME, Long).size(size);
            }
            Signal::Short => {
                strategy.close(NAME);
            }
            _ => {}
        }
        if strategy.position() > 0.0 {
            let cost = strategy.avg_cost();
            strategy
                .exit("bracket")
                .from(NAME)
                .take(cost * (1.0 + TARGET))
                .stop(cost * (1.0 - STOP));
        }
    }
}
