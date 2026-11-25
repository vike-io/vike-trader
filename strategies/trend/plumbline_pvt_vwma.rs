// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-plumbline-pvt-vwma-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 2 when flat on the first bar with the close above VWMA(33) and the price volume trend above its EMA(41). Closed when either
//! falls below, or the close is more than 2.2 ATR(21) under the average cost. 0.05% commission, 1 tick.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::Ema;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "long";
const SIZE: f64 = 2.0;
const MOST_ATRS_AGAINST: f64 = 2.2;

struct S {
    centre: IndicatorLine,
    flow: IndicatorLine,
    flow_average: Ema,
    atr: IndicatorLine,
    was_confirmed: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        centre: IndicatorLine::new("vwma", &[33.0]),
        flow: IndicatorLine::new("pvt", &[]),
        flow_average: Ema::new(41.0).expect("a valid period"),
        atr: IndicatorLine::new("atr", &[21.0]),
        was_confirmed: false,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let centre = self.centre.next(bar)[0];
        let flow = self.flow.next(bar)[0];
        let flow_average = self.flow_average.next_value(flow);
        let atr = self.atr.next(bar)[0];
        let confirmed = bar.close > centre && flow > flow_average;
        let was_confirmed = std::mem::replace(&mut self.was_confirmed, confirmed);
        let held = strategy.position();
        let ended = bar.close < centre || flow < flow_average;
        let too_far = held > 0.0 && bar.close < strategy.avg_cost() - atr * MOST_ATRS_AGAINST;
        if held == 0.0 && confirmed && !was_confirmed {
            strategy.entry(NAME, Long).size(SIZE);
        } else if held > 0.0 && (ended || too_far) {
            strategy.close(NAME);
        }
    }
}
