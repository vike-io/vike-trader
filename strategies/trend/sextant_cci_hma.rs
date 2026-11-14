// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-sextant-cci-hma-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 2 when flat, the CCI(21) of the bar's typical price crosses up through 72, HMA(34) is rising and the close is above it.
//! Closed when the CCI crosses down through -18, the close is below the HMA, or the close is more than 2.3 ATR(15) under the
//! average cost. 0.05% commission, 1 tick.
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
const ENTER_ABOVE: f64 = 72.0;
const LEAVE_BELOW: f64 = -18.0;
const MOST_ATRS_AGAINST: f64 = 2.3;

struct S {
    cci: IndicatorLine,
    hma: IndicatorLine,
    atr: IndicatorLine,
    last_hma: f64,
    up: Cross,
    down: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        cci: IndicatorLine::new("cci", &[21.0]),
        hma: IndicatorLine::new("hma", &[34.0]),
        atr: IndicatorLine::new("atr", &[15.0]),
        last_hma: f64::NAN,
        up: Cross::new(),
        down: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let cci = self.cci.next(bar)[0];
        let hma = self.hma.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        let rising = hma > self.last_hma;
        self.last_hma = hma;
        let crossed_up = matches!(self.up.step(cci, ENTER_ABOVE), Signal::Long);
        let crossed_down = matches!(self.down.step(cci, LEAVE_BELOW), Signal::Short);
        let enter = crossed_up && rising && bar.close > hma;
        let ended = crossed_down || bar.close < hma;
        let held = strategy.position();
        let too_far = held > 0.0 && bar.close < strategy.avg_cost() - atr * MOST_ATRS_AGAINST;
        if held == 0.0 && enter {
            strategy.entry(NAME, Long).size(SIZE);
        } else if held > 0.0 && (ended || too_far) {
            strategy.close(NAME);
        }
    }
}
