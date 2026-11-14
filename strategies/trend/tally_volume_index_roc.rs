// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-tally-volume-index-roc-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 2 when flat and the 83-bar percent change of the negative volume index above 0.55 and the 61-bar percent change of the
//! positive volume index above 0.35 become true together this bar, with the close above EMA(53). Closed when the first is below
//! -0.275, the second below -0.175, the close below the EMA, or the close more than 2.6 ATR(20) under the average cost. 0.05%
//! commission, 1 tick.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::Roc;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const NAME: &str = "long";
const SIZE: f64 = 2.0;
const MIN_NEGATIVE_GROWTH: f64 = 0.55;
const MIN_POSITIVE_GROWTH: f64 = 0.35;
const LEAVE_SHARE: f64 = 0.5;
const MOST_ATRS_AGAINST: f64 = 2.6;

struct S {
    negative: IndicatorLine,
    positive: IndicatorLine,
    negative_growth: Roc,
    positive_growth: Roc,
    trend: IndicatorLine,
    atr: IndicatorLine,
    agreed: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        negative: IndicatorLine::new("nvi", &[]),
        positive: IndicatorLine::new("pvi", &[]),
        negative_growth: Roc::new(83.0).expect("a valid period"),
        positive_growth: Roc::new(61.0).expect("a valid period"),
        trend: IndicatorLine::new("ema", &[53.0]),
        atr: IndicatorLine::new("atr", &[20.0]),
        agreed: false,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let negative = self.negative_growth.next_value(self.negative.next(bar)[0]);
        let positive = self.positive_growth.next_value(self.positive.next(bar)[0]);
        let trend = self.trend.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        let agree = negative > MIN_NEGATIVE_GROWTH && positive > MIN_POSITIVE_GROWTH;
        let enter = agree && !self.agreed && bar.close > trend;
        self.agreed = agree;
        let ended = negative < -MIN_NEGATIVE_GROWTH * LEAVE_SHARE
            || positive < -MIN_POSITIVE_GROWTH * LEAVE_SHARE
            || bar.close < trend;
        let held = strategy.position();
        let too_far = held > 0.0 && bar.close < strategy.avg_cost() - atr * MOST_ATRS_AGAINST;
        if held == 0.0 && enter {
            strategy.entry(NAME, Long).size(SIZE);
        } else if held > 0.0 && (ended || too_far) {
            strategy.close(NAME);
        }
    }
}
