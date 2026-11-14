// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-crosscurrent-opposite-market-pair-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 8% of the account's worth when RSI(17) crosses up through 58, short 8% when it crosses down through 42, one lot at most, an
//! opposite position closing at the same open. While held, a stop 2.4 ATR(14) past the average cost, moved every bar. 0.05%
//! commission, 1 tick. Starts with 100,000.
//!
//! The rule also asks for a close of the opposite side in the same pass. It fills at the same open as the entry and the new lot
//! is the entry's own size, which is what one reversing entry does, so no separate close is issued.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{CrossBands, Signal};
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const LONG_LOT: &str = "long";
const SHORT_LOT: &str = "short";
const SHARE: f64 = 0.08;
const STOP_ATRS: f64 = 2.4;

struct S {
    rsi: IndicatorLine,
    atr: IndicatorLine,
    turns: CrossBands,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        rsi: IndicatorLine::new("rsi", &[17.0]),
        atr: IndicatorLine::new("atr", &[14.0]),
        turns: CrossBands::new(58.0, 42.0),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let rsi = self.rsi.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        let held = strategy.position();
        match self.turns.step(rsi) {
            Signal::Long => {
                let size = sizing.size_for_share(SHARE, sizing.slipped(Long, bar.close));
                strategy.entry(LONG_LOT, Long).size(size);
            }
            Signal::Short => {
                let size = sizing.size_for_share(SHARE, sizing.slipped(Short, bar.close));
                strategy.entry(SHORT_LOT, Short).size(size);
            }
            _ => {}
        }
        if held > 0.0 {
            let cut = strategy.avg_cost() - atr * STOP_ATRS;
            strategy.exit("long stop").from(LONG_LOT).stop(cut);
        }
        if held < 0.0 {
            let cut = strategy.avg_cost() + atr * STOP_ATRS;
            strategy.exit("short stop").from(SHORT_LOT).stop(cut);
        }
    }
}
