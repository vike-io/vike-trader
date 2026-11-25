// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/pyramid-terrace-staged-entry-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Adds a long of 1, up to three lots, while EMA(42) is rising and the close is above it: the first stage when the close crosses up
//! through EMA + 0.25 ATR(16), the second at a position of 1 when it crosses down through the EMA, the third at a position of 2 when
//! it crosses down through EMA - 0.70 ATR. While held, a stop 2.8 ATR under the average cost, moved every bar, and everything closes
//! at the next open once the EMA falls. 0.05% commission, 1 tick. Starts with 100,000.
//!
//! The gate needs the close above the EMA, so the two downward crosses can never fire and every lot carries the first stage's
//! name. Each cross only remembers the bars its gate let through.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const FIRST: &str = "first";
const SECOND: &str = "second";
const THIRD: &str = "third";
const MOST_LOTS: usize = 3;
const FIRST_ATRS: f64 = 0.25;
const THIRD_ATRS: f64 = 0.70;
const STOP_ATRS: f64 = 2.8;

struct S {
    trend: IndicatorLine,
    atr: IndicatorLine,
    trend_before: f64,
    first_cross: Cross,
    second_cross: Cross,
    third_cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        trend: IndicatorLine::new("ema", &[42.0]),
        atr: IndicatorLine::new("atr", &[16.0]),
        trend_before: f64::NAN,
        first_cross: Cross::new(),
        second_cross: Cross::new(),
        third_cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults {
            fee: Commission::percent(0.05),
            pyramiding: MOST_LOTS,
            ..RuleDefaults::PLAIN
        }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let trend = self.trend.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        let trend_before = std::mem::replace(&mut self.trend_before, trend);
        let rising = trend > trend_before && bar.close > trend;
        let held = strategy.position();

        let first = trend + atr * FIRST_ATRS;
        if rising && matches!(self.first_cross.step(bar.close, first), Signal::Long) {
            strategy.entry(FIRST, Long).size(1.0);
        }
        if rising
            && held == 1.0
            && matches!(self.second_cross.step(bar.close, trend), Signal::Short)
        {
            strategy.entry(SECOND, Long).size(1.0);
        }
        let third = trend - atr * THIRD_ATRS;
        if rising && held == 2.0 && matches!(self.third_cross.step(bar.close, third), Signal::Short)
        {
            strategy.entry(THIRD, Long).size(1.0);
        }

        if held > 0.0 {
            let cut = strategy.avg_cost() - atr * STOP_ATRS;
            strategy.exit("stop").from(FIRST).stop(cut);
            if trend < trend_before {
                strategy.close(FIRST);
                strategy.close(SECOND);
                strategy.close(THIRD);
            }
        }
    }
}
