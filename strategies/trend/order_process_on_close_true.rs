// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-process-on-close-true-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 1 when flat, EMA(9) crosses up through EMA(21) and RSI(14) is under 70; closed when EMA(9) crosses down. Every order
//! fills at the close of the bar that signalled.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! fill = "close"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "long";
const RSI_BELOW: f64 = 70.0;

struct S {
    rsi: IndicatorLine,
    fast: IndicatorLine,
    slow: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        rsi: IndicatorLine::new("rsi", &[14.0]),
        fast: IndicatorLine::new("ema", &[9.0]),
        slow: IndicatorLine::new("ema", &[21.0]),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let rsi = self.rsi.next(bar)[0];
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        let cross = self.cross.step(fast, slow);
        if matches!(cross, Signal::Long) && rsi < RSI_BELOW && strategy.position() == 0.0 {
            strategy.entry(NAME, Long).size(1.0);
        }
        if matches!(cross, Signal::Short) && strategy.position() > 0.0 {
            strategy.close(NAME);
        }
    }
}
