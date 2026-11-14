// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-keystone-udf-tuple-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! A channel of EMA(27) with a width of 1.6 ATR(27). Long 7% of the account's worth when the close crosses up through the upper band,
//! closed when it crosses down through the centre. While held, a stop one width under the average cost and a target 2.1 widths above
//! it. 0.05% commission, 1 tick.
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
const SHARE: f64 = 0.07;
const LENGTH: f64 = 27.0;
const WIDTH_ATRS: f64 = 1.6;
const TARGET_WIDTHS: f64 = 2.1;

struct S {
    centre: IndicatorLine,
    atr: IndicatorLine,
    above: Cross,
    through_centre: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        centre: IndicatorLine::new("ema", &[LENGTH]),
        atr: IndicatorLine::new("atr", &[LENGTH]),
        above: Cross::new(),
        through_centre: Cross::new(),
    })
}

/// `(centre, upper, width)`.
fn channel(centre: f64, atr: f64) -> (f64, f64, f64) {
    let width = atr * WIDTH_ATRS;
    (centre, centre + width, width)
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let (centre, upper, width) = channel(self.centre.next(bar)[0], self.atr.next(bar)[0]);
        if matches!(self.above.step(bar.close, upper), Signal::Long) {
            strategy
                .entry(NAME, Long)
                .size(sizing.size_for_share(SHARE, sizing.slipped(Long, bar.close)));
        }
        if matches!(self.through_centre.step(bar.close, centre), Signal::Short) {
            strategy.close(NAME);
        }
        if strategy.position() > 0.0 {
            let cost = strategy.avg_cost();
            strategy.exit("risk").from(NAME).take(cost + width * TARGET_WIDTHS).stop(cost - width);
        }
    }
}
