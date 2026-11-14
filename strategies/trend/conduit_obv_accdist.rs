// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-conduit-obv-accdist-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 2 when flat and on-balance volume above its EMA(29) and the accumulation/distribution line above its EMA(37) become true
//! together this bar, with the close above EMA(52). Closed when they stop agreeing, the close is below the EMA, or the close is
//! more than 2.4 ATR(16) under the average cost. 0.05% commission, 1 tick.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::Ema;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const NAME: &str = "long";
const SIZE: f64 = 2.0;
const MOST_ATRS_AGAINST: f64 = 2.4;

struct S {
    obv: IndicatorLine,
    flow: IndicatorLine,
    obv_signal: Ema,
    flow_signal: Ema,
    filter: IndicatorLine,
    atr: IndicatorLine,
    agreed: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        obv: IndicatorLine::new("obv", &[]),
        flow: IndicatorLine::new("ad", &[]),
        obv_signal: Ema::new(29.0).expect("a valid period"),
        flow_signal: Ema::new(37.0).expect("a valid period"),
        filter: IndicatorLine::new("ema", &[52.0]),
        atr: IndicatorLine::new("atr", &[16.0]),
        agreed: false,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let obv = self.obv.next(bar)[0];
        let flow = self.flow.next(bar)[0];
        let obv_signal = self.obv_signal.next_value(obv);
        let flow_signal = self.flow_signal.next_value(flow);
        let agree = obv > obv_signal && flow > flow_signal;
        let filter = self.filter.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        let enter = agree && !self.agreed && bar.close > filter;
        self.agreed = agree;
        let ended = !agree || bar.close < filter;
        let held = strategy.position();
        let too_far = held > 0.0 && bar.close < strategy.avg_cost() - atr * MOST_ATRS_AGAINST;
        if held == 0.0 && enter {
            strategy.entry(NAME, Long).size(SIZE);
        } else if held > 0.0 && (ended || too_far) {
            strategy.close(NAME);
        }
    }
}
