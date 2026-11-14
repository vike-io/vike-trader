// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-quarry-alma-correlation-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 2 when flat, the close crosses up through ALMA(31, offset 0.72, sigma 5.4) and the 23-bar correlation of close and volume
//! is above 0.04. Closed when the close crosses down through the ALMA, the correlation is below -0.04, or the close is more than
//! 2.6 ATR(17) under the average cost. 0.05% commission, 1 tick.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::pairs::Correl;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const NAME: &str = "long";
const SIZE: f64 = 2.0;
const MIN_CORRELATION: f64 = 0.04;
const MOST_ATRS_AGAINST: f64 = 2.6;

struct S {
    alma: IndicatorLine,
    correlation: Correl,
    atr: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        alma: IndicatorLine::new("alma", &[31.0, 0.72, 5.4]),
        correlation: Correl::with_params(&[23.0]),
        atr: IndicatorLine::new("atr", &[17.0]),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let alma = self.alma.next(bar)[0];
        let correlation = self.correlation.on_pair_values(bar.close, bar.volume);
        let atr = self.atr.next(bar)[0];
        let crossed = self.cross.step(bar.close, alma);
        let enter = matches!(crossed, Signal::Long) && correlation > MIN_CORRELATION;
        let ended = matches!(crossed, Signal::Short) || correlation < -MIN_CORRELATION;
        let held = strategy.position();
        let too_far = held > 0.0 && bar.close < strategy.avg_cost() - atr * MOST_ATRS_AGAINST;
        if held == 0.0 && enter {
            strategy.entry(NAME, Long).size(SIZE);
        } else if held > 0.0 && (ended || too_far) {
            strategy.close(NAME);
        }
    }
}
