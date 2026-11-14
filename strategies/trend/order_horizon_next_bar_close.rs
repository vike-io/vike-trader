// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-horizon-next-bar-close-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 6% of the account's worth when flat and the close crosses up through SMA(34). While held, a stop 2.2 ATR(14) under the
//! average cost, moved every bar, and the lot closes at the open after the 11th bar past its entry bar. 0.05% commission, 1 tick.
//! Starts with 100,000.
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
const HOLD_BARS: i64 = 11;
const STOP_ATRS: f64 = 2.2;

struct S {
    average: IndicatorLine,
    atr: IndicatorLine,
    cross: Cross,
    bar_index: i64,
    opened_at: Option<i64>,
    held_before: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        average: IndicatorLine::new("sma", &[34.0]),
        atr: IndicatorLine::new("atr", &[14.0]),
        cross: Cross::new(),
        bar_index: 0,
        opened_at: None,
        held_before: 0.0,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let average = self.average.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        let crossed_up = matches!(self.cross.step(bar.close, average), Signal::Long);
        let held = strategy.position();
        if crossed_up && held == 0.0 {
            strategy
                .entry(NAME, Long)
                .size(sizing.size_for_share(SHARE, sizing.slipped(Long, bar.close)));
        }
        if held > 0.0 && self.held_before <= 0.0 {
            self.opened_at = Some(self.bar_index);
        }
        if held > 0.0 {
            if self.opened_at.is_some_and(|at| self.bar_index - at >= HOLD_BARS) {
                strategy.close(NAME);
            }
            let cut = strategy.avg_cost() - atr * STOP_ATRS;
            strategy.exit("stop").from(NAME).stop(cut);
        } else {
            self.opened_at = None;
        }
        self.held_before = held;
        self.bar_index += 1;
    }
}
