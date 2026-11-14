// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/session-compass-utc-market-window-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 7% of the account's worth inside the 08:00 to 16:00 UTC session, on its first bar or when EMA(16) crosses up through
//! EMA(44). Closed on the first bar after the session. While held, a stop 2 ATR(14) under the average cost. 0.05% commission,
//! 1 tick.
//!
//! The cross is only asked inside the session and after its first bar, so that is all it remembers.
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
const SHARE: f64 = 0.07;
const STOP_ATRS: f64 = 2.0;

struct S {
    fast: IndicatorLine,
    slow: IndicatorLine,
    atr: IndicatorLine,
    cross: Cross,
    was_inside: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: IndicatorLine::new("ema", &[16.0]),
        slow: IndicatorLine::new("ema", &[44.0]),
        atr: IndicatorLine::new("atr", &[14.0]),
        cross: Cross::new(),
        was_inside: false,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        let inside = (8..16).contains(&clock(bar).0);
        let was_inside = std::mem::replace(&mut self.was_inside, inside);
        let enter = inside && (!was_inside || matches!(self.cross.step(fast, slow), Signal::Long));
        if enter {
            strategy
                .entry(NAME, Long)
                .size(sizing.size_for_share(SHARE, sizing.slipped(Long, bar.close)));
        }
        if was_inside && !inside {
            strategy.close(NAME);
        }
        if strategy.position() > 0.0 {
            let cut = strategy.avg_cost() - atr * STOP_ATRS;
            strategy.exit("guard").from(NAME).stop(cut);
        }
    }
}
