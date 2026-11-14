// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/barstate-lattice-confirmed-pivot-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 2 when EMA(17) crosses up through EMA(53) with RSI(13) above 54. Closed when it crosses down, RSI is under 44, or 192 bars have
//! passed since the entry bar. While held, a stop 2.3 ATR(15) under the average cost, moved every bar. 0.05% commission, 1 tick.
//! Starts with 100,000.
//!
//! The rule only acts on a confirmed bar, and every bar of a historical run is one.
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
const ENTRY_RSI: f64 = 54.0;
const EXIT_RSI: f64 = 44.0;
const STOP_ATRS: f64 = 2.3;
const MOST_BARS: i64 = 192;

struct S {
    fast: IndicatorLine,
    slow: IndicatorLine,
    rsi: IndicatorLine,
    atr: IndicatorLine,
    cross: Cross,
    bar_index: i64,
    opened_at: Option<i64>,
    held_before: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: IndicatorLine::new("ema", &[17.0]),
        slow: IndicatorLine::new("ema", &[53.0]),
        rsi: IndicatorLine::new("rsi", &[13.0]),
        atr: IndicatorLine::new("atr", &[15.0]),
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
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        let rsi = self.rsi.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        let held = strategy.position();
        if held > 0.0 && self.held_before == 0.0 {
            self.opened_at = Some(self.bar_index);
        }
        let turn = self.cross.step(fast, slow);
        let leaving = matches!(turn, Signal::Short) || rsi < EXIT_RSI;
        let expired =
            held > 0.0 && self.opened_at.is_some_and(|at| self.bar_index - at >= MOST_BARS);
        if matches!(turn, Signal::Long) && rsi > ENTRY_RSI {
            strategy.entry(NAME, Long).size(SIZE);
        }
        if held > 0.0 {
            if leaving || expired {
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
