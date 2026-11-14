// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-tranche-partial-market-close-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 4 when flat and EMA(19) crosses up through EMA(58). Half closes once, the first time the close reaches 1.3 ATR(16) above the
//! average cost. Otherwise what is held closes when the fast average crosses down through the slow one, or the close is more than
//! 2.4 ATR(16) under the average cost. 0.05% commission, 1 tick.
//!
//! The downward cross only remembers bars on which the long is held.
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
const SIZE: f64 = 4.0;
const TRANCHE_SHARE: f64 = 0.5;
const TRANCHE_ATRS: f64 = 1.3;
const MOST_ATRS_AGAINST: f64 = 2.4;

struct S {
    fast: IndicatorLine,
    slow: IndicatorLine,
    atr: IndicatorLine,
    up: Cross,
    down: Cross,
    tranche_taken: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: IndicatorLine::new("ema", &[19.0]),
        slow: IndicatorLine::new("ema", &[58.0]),
        atr: IndicatorLine::new("atr", &[16.0]),
        up: Cross::new(),
        down: Cross::new(),
        tranche_taken: false,
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
        let crossed_up = matches!(self.up.step(fast, slow), Signal::Long);
        let held = strategy.position();
        if held == 0.0 {
            self.tranche_taken = false;
        }
        let cost = strategy.avg_cost();
        let reached = held > 0.0 && bar.close >= cost + atr * TRANCHE_ATRS;
        let turned = held > 0.0 && matches!(self.down.step(fast, slow), Signal::Short);
        let too_far = held > 0.0 && bar.close < cost - atr * MOST_ATRS_AGAINST;
        if held == 0.0 && crossed_up {
            strategy.entry(NAME, Long).size(SIZE);
        } else if held > 0.0 && !self.tranche_taken && reached {
            strategy.close(NAME).size(held * TRANCHE_SHARE);
            self.tranche_taken = true;
        } else if held > 0.0 && (turned || too_far) {
            strategy.close(NAME);
        }
    }
}
