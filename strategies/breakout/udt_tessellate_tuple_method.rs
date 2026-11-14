// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/udt-tessellate-tuple-method-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! A channel around EMA(42), 1.25 ATR(17) above and 0.8 ATR(17) below. Long 2 when flat and the close crosses up through the top,
//! closed when the close is under the bottom or more than 2.5 ATR(17) under the average cost. 0.05% commission, 1 tick.
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
const ATRS_ABOVE: f64 = 1.25;
const ATRS_BELOW: f64 = 0.8;
const MOST_ATRS_AGAINST: f64 = 2.5;

struct Channel {
    middle: f64,
    atr: f64,
}

impl Channel {
    fn edges(&self, above: f64, below: f64) -> (f64, f64) {
        (self.middle + self.atr * above, self.middle - self.atr * below)
    }
}

struct S {
    middle: IndicatorLine,
    atr: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        middle: IndicatorLine::new("ema", &[42.0]),
        atr: IndicatorLine::new("atr", &[17.0]),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let channel = Channel { middle: self.middle.next(bar)[0], atr: self.atr.next(bar)[0] };
        let (top, bottom) = channel.edges(ATRS_ABOVE, ATRS_BELOW);
        let enter = matches!(self.cross.step(bar.close, top), Signal::Long);
        let held = strategy.position();
        let too_far =
            held > 0.0 && bar.close < strategy.avg_cost() - channel.atr * MOST_ATRS_AGAINST;
        if held == 0.0 && enter {
            strategy.entry(NAME, Long).size(SIZE);
        } else if held > 0.0 && (bar.close < bottom || too_far) {
            strategy.close(NAME);
        }
    }
}
