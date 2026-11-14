// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/map-mosaic-regime-weight-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! A regime weight is 1.15 when ATR(18) is at least 1.8% of the close, otherwise 0.85. The distance is the close less EMA(33), in
//! ATRs, times that weight. Long 5% of the account's worth when the distance crosses up through 0.65, closed when it is below -0.15.
//! While held, a stop 2.6 ATR over the weight under the average cost. 0.05% commission, 1 tick.
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
const SHARE: f64 = 0.05;
const EXPANSION_PERCENT: f64 = 1.8;
const ACTIVE_WEIGHT: f64 = 1.15;
const CALM_WEIGHT: f64 = 0.85;
const ENTRY_DISTANCE: f64 = 0.65;
const FAILED_DISTANCE: f64 = -0.15;
const STOP_ATRS: f64 = 2.6;

struct S {
    center: IndicatorLine,
    atr: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        center: IndicatorLine::new("ema", &[33.0]),
        atr: IndicatorLine::new("atr", &[18.0]),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let center = self.center.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        let weight =
            if 100.0 * atr / bar.close >= EXPANSION_PERCENT { ACTIVE_WEIGHT } else { CALM_WEIGHT };
        let distance = (bar.close - center) / atr * weight;
        if matches!(self.cross.step(distance, ENTRY_DISTANCE), Signal::Long) {
            strategy
                .entry(NAME, Long)
                .size(sizing.size_for_share(SHARE, sizing.slipped(Long, bar.close)));
        }
        let held = strategy.position() > 0.0;
        if held && distance < FAILED_DISTANCE {
            strategy.close(NAME);
        }
        if held {
            let cut = strategy.avg_cost() - atr * STOP_ATRS / weight;
            strategy.exit("guard").from(NAME).stop(cut);
        }
    }
}
