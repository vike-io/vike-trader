// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/math-forge-log-channel-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! A channel over the 24 bars before this one, its high and low each pushed out by 0.18 of the mean absolute log change of the close
//! across those bars. Long 6% of the account's worth when the close crosses up through the upper trigger, short 6% when it crosses
//! down through the lower one, reversing what is held. While held, a stop 2.3 ATR(16) from the average cost. 0.05% commission, 1 tick.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use std::collections::VecDeque;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;

const UP: &str = "long";
const DOWN: &str = "short";
const SHARE: f64 = 0.06;
const LENGTH: usize = 24;
const LOG_BUFFER: f64 = 0.18;
const STOP_ATRS: f64 = 2.3;

struct S {
    /// The last `LENGTH` bars, newest first.
    past: VecDeque<Bar>,
    atr: IndicatorLine,
    above: Cross,
    below: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        past: VecDeque::with_capacity(LENGTH + 1),
        atr: IndicatorLine::new("atr", &[16.0]),
        above: Cross::new(),
        below: Cross::new(),
    })
}

impl S {
    /// `(upper, lower)`, NaN until `LENGTH` bars have passed.
    fn triggers(&self) -> (f64, f64) {
        if self.past.len() < LENGTH {
            return (f64::NAN, f64::NAN);
        }
        let (mut high, mut low) = (self.past[0].high, self.past[0].low);
        let mut moved = 0.0;
        for offset in 1..LENGTH {
            let (newer, older) = (&self.past[offset - 1], &self.past[offset]);
            high = high.max(older.high);
            low = low.min(older.low);
            moved += libm::log(newer.close / older.close).abs();
        }
        let buffer = moved / (LENGTH - 1) as f64 * LOG_BUFFER;
        (high * (1.0 + buffer), low * (1.0 - buffer))
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let (upper, lower) = self.triggers();
        self.past.push_front(bar.clone());
        self.past.truncate(LENGTH);
        let atr = self.atr.next(bar)[0];
        if matches!(self.above.step(bar.close, upper), Signal::Long) {
            strategy
                .entry(UP, Long)
                .size(sizing.size_for_share(SHARE, sizing.slipped(Long, bar.close)));
        }
        if matches!(self.below.step(bar.close, lower), Signal::Short) {
            strategy
                .entry(DOWN, Short)
                .size(sizing.size_for_share(SHARE, sizing.slipped(Short, bar.close)));
        }
        let held = strategy.position();
        let distance = atr * STOP_ATRS;
        if held > 0.0 {
            let cut = strategy.avg_cost() - distance;
            strategy.exit("long guard").from(UP).stop(cut);
        }
        if held < 0.0 {
            let cut = strategy.avg_cost() + distance;
            strategy.exit("short guard").from(DOWN).stop(cut);
        }
    }
}
