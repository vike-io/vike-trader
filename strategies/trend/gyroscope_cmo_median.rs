// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-gyroscope-cmo-median-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 2 when flat and the close crosses up through its 27-bar median with CMO(16) above 12. Closed when the close is below
//! the median, CMO(16) is below -9, or the close is more than 2.1 ATR(19) under the average cost. 0.05% commission, 1 tick.
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
const ENTER_ABOVE: f64 = 12.0;
const LEAVE_BELOW: f64 = -9.0;
const MOST_ATRS_AGAINST: f64 = 2.1;

struct S {
    median: IndicatorLine,
    cmo: IndicatorLine,
    atr: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        median: IndicatorLine::new("median_ma", &[27.0]),
        cmo: IndicatorLine::new("cmo", &[16.0]),
        atr: IndicatorLine::new("atr", &[19.0]),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let median = self.median.next(bar)[0];
        let cmo = self.cmo.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        let crossed_up = matches!(self.cross.step(bar.close, median), Signal::Long);
        let held = strategy.position();
        let enter = crossed_up && cmo > ENTER_ABOVE;
        let ended = bar.close < median || cmo < LEAVE_BELOW;
        let too_far = held > 0.0 && bar.close < strategy.avg_cost() - atr * MOST_ATRS_AGAINST;
        if held == 0.0 && enter {
            strategy.entry(NAME, Long).size(SIZE);
        } else if held > 0.0 && (ended || too_far) {
            strategy.close(NAME);
        }
    }
}
