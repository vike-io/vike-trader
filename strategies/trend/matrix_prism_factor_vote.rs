// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/matrix-prism-factor-vote-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Four factors each vote 1 or -1: EMA(16) above EMA(44), the close above EMA(16), RSI(13) above 52, ATR(19) under 4% of the close.
//! Long 6% of the account's worth when their sum crosses up through 2, closed when it is 0 or less. While held, a stop 2.2 ATR under
//! the average cost. 0.05% commission, 1 tick.
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
const SHARE: f64 = 0.06;
const LEAST_RSI: f64 = 52.0;
const MOST_ATR_SHARE: f64 = 0.04;
const MINIMUM_SCORE: f64 = 2.0;
const STOP_ATRS: f64 = 2.2;

struct S {
    fast: IndicatorLine,
    slow: IndicatorLine,
    rsi: IndicatorLine,
    atr: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: IndicatorLine::new("ema", &[16.0]),
        slow: IndicatorLine::new("ema", &[44.0]),
        rsi: IndicatorLine::new("rsi", &[13.0]),
        atr: IndicatorLine::new("atr", &[19.0]),
        cross: Cross::new(),
    })
}

fn vote(agrees: bool) -> f64 {
    if agrees { 1.0 } else { -1.0 }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        let rsi = self.rsi.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        let grid = [
            [vote(fast > slow), vote(bar.close > fast)],
            [vote(rsi > LEAST_RSI), vote(atr / bar.close < MOST_ATR_SHARE)],
        ];
        let score: f64 = grid.iter().flatten().sum();
        if matches!(self.cross.step(score, MINIMUM_SCORE), Signal::Long) {
            strategy
                .entry(NAME, Long)
                .size(sizing.size_for_share(SHARE, sizing.slipped(Long, bar.close)));
        }
        let held = strategy.position() > 0.0;
        if held && score <= 0.0 {
            strategy.close(NAME);
        }
        if held {
            let cut = strategy.avg_cost() - atr * STOP_ATRS;
            strategy.exit("guard").from(NAME).stop(cut);
        }
    }
}
