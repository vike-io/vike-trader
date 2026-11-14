// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/pyramid-cash-fractional-commission-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Adds a long of 50,000 in cash at the signal bar's close, up to three lots, when EMA(9) crosses up through EMA(21) and RSI(14) is
//! between 40 and 70. The whole position closes at the open after 23:45. Costs: 0.05 per contract on each fill, on fractional sizes.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::calendar::clock;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const NAME: &str = "long";
const CASH: f64 = 50_000.0;
const RSI_ABOVE: f64 = 40.0;
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
        RuleDefaults {
            fee: RuleDefaultsFee::PerContract(0.05),
            pyramiding: 3,
            ..RuleDefaults::PLAIN
        }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let rsi = self.rsi.next(bar)[0];
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        let up = matches!(self.cross.step(fast, slow), Signal::Long);
        if up && rsi > RSI_ABOVE && rsi < RSI_BELOW {
            strategy.entry(NAME, Long).size(CASH / sizing.slipped(Long, bar.close));
        }
        if clock(bar) == (23, 45) {
            strategy.close_all();
        }
    }
}
