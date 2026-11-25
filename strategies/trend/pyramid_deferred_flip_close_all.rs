// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/pyramid-deferred-flip-close-all-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Up to four lots a side, asked size 1. A market long when EMA(9) crosses up through EMA(21) with RSI(14) under 70. A sell stop a
//! quarter ATR(14) under the bar's low when RSI(14) crosses down through 50, sized with any market long issued in the same pass.
//! The whole position closes at the open after the 21:45 UTC bar, and a stop still resting stays.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::{Atr, Ema, Rsi};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::calendar::clock;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const ADD: &str = "add long";
const FLIP: &str = "flip short";
const LOTS_A_SIDE: usize = 4;
const RSI_BELOW: f64 = 70.0;
const RSI_MIDDLE: f64 = 50.0;
const STOP_ATRS: f64 = 0.25;

struct DeferredFlip {
    rsi: Rsi,
    fast: Ema,
    slow: Ema,
    atr: Atr,
    trend_cross: Cross,
    rsi_cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(DeferredFlip {
        rsi: Rsi::new(14.0).expect("a valid period"),
        fast: Ema::new(9.0).expect("a valid period"),
        slow: Ema::new(21.0).expect("a valid period"),
        atr: Atr::new(14.0).expect("a valid period"),
        trend_cross: Cross::new(),
        rsi_cross: Cross::new(),
    })
}

impl Rule for DeferredFlip {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { pyramiding: LOTS_A_SIDE, ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let rsi = self.rsi.next(bar);
        let fast = self.fast.next(bar);
        let slow = self.slow.next(bar);
        let atr = self.atr.next(bar);
        let trend = self.trend_cross.step(fast, slow);
        let momentum = self.rsi_cross.step(rsi, RSI_MIDDLE);

        // A market long issued in this pass fills first, and the engine sizes the stop below against the position it leaves.
        if matches!(trend, Signal::Long) && rsi < RSI_BELOW {
            strategy.entry(ADD, Long).size(1.0);
        }
        if matches!(momentum, Signal::Short) {
            strategy.entry(FLIP, Short).size(1.0).stop(bar.low - atr * STOP_ATRS);
        }
        if clock(bar) == (21, 45) {
            strategy.close_all();
        }
    }
}
