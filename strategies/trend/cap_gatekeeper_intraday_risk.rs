// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/cap-gatekeeper-intraday-risk-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 2, filling at this close, when EMA(10) crosses up through EMA(31) inside 09:00 to 17:00 in Taipei, which is 01:00 to 09:00
//! UTC. The cross only remembers bars inside that session. Outside it, what is held closes. While held, a stop 1.8 ATR(14) under the
//! average cost, on every bar. Four fills a UTC day. 0.05% commission, 1 tick, 100,000 of cash.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! fill = "close"
//!
//! [run]
//! fills_a_day = 4
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::calendar::clock;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "long";
const STOP_ATRS: f64 = 1.8;
const SIZE: f64 = 2.0;

struct S {
    fast: IndicatorLine,
    slow: IndicatorLine,
    atr: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: IndicatorLine::new("ema", &[10.0]),
        slow: IndicatorLine::new("ema", &[31.0]),
        atr: IndicatorLine::new("atr", &[14.0]),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        let in_session = (1..9).contains(&clock(bar).0);
        if in_session && matches!(self.cross.step(fast, slow), Signal::Long) {
            strategy.entry(NAME, Long).size(SIZE);
        }
        if strategy.position() > 0.0 {
            if !in_session {
                strategy.close(NAME);
            }
            let cut = strategy.avg_cost() - atr * STOP_ATRS;
            strategy.exit("stop").from(NAME).stop(cut);
        }
    }
}
