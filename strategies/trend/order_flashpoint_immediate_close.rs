// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-flashpoint-immediate-close-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 2 when flat and the 12-bar rate of change crosses up through 0.6 with the close above EMA(47). A one-bar return of -2.4%
//! or worse closes at this bar's close; otherwise a close below the EMA closes at the next open. 0.05% commission, 1 tick.
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
const ENTER_RATE: f64 = 0.6;
const SHOCK_RATE: f64 = -2.4;

struct S {
    trend: IndicatorLine,
    rate: IndicatorLine,
    one_bar: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        trend: IndicatorLine::new("ema", &[47.0]),
        rate: IndicatorLine::new("roc", &[12.0]),
        one_bar: IndicatorLine::new("roc", &[1.0]),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let trend = self.trend.next(bar)[0];
        let rate = self.rate.next(bar)[0];
        let one_bar = self.one_bar.next(bar)[0];
        let crossed_up = matches!(self.cross.step(rate, ENTER_RATE), Signal::Long);
        let held = strategy.position();
        if held == 0.0 && crossed_up && bar.close > trend {
            strategy.entry(NAME, Long).size(SIZE);
        } else if held > 0.0 && one_bar <= SHOCK_RATE {
            strategy.close(NAME).here();
        } else if held > 0.0 && bar.close < trend {
            strategy.close(NAME);
        }
    }
}
