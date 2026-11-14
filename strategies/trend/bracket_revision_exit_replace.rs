// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/bracket-revision-exit-replace-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 2 when flat and EMA(20) crosses up through EMA(61). While held, one bracket around the average cost under one name, moved on
//! every bar: a stop 2.8 ATR(17) under and a target 4.2 ATR(17) above for the first 14 bars the rule sees the lot, then a stop 1.4
//! and a target 2.4 ATR(17) away. Closes at the next open when the fast average crosses down. 0.05% commission, 1 tick, 100,000 of
//! cash.
//!
//! The downward cross only remembers bars on which the long is held.
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
const WIDE_HOLD_BARS: i64 = 14;

struct S {
    fast: IndicatorLine,
    slow: IndicatorLine,
    atr: IndicatorLine,
    up: Cross,
    down: Cross,
    /// Bars the rule has seen this lot, 0 on the first.
    seen: Option<i64>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: IndicatorLine::new("ema", &[20.0]),
        slow: IndicatorLine::new("ema", &[61.0]),
        atr: IndicatorLine::new("atr", &[17.0]),
        up: Cross::new(),
        down: Cross::new(),
        seen: None,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        let crossed_up = matches!(self.up.step(fast, slow), Signal::Long);
        let held = strategy.position();
        if held == 0.0 && crossed_up {
            strategy.entry(NAME, Long).size(2.0);
        }
        self.seen = (held > 0.0).then(|| self.seen.map_or(0, |bars| bars + 1));
        if let Some(bars) = self.seen {
            let cost = strategy.avg_cost();
            let (stop_atrs, target_atrs) =
                if bars < WIDE_HOLD_BARS { (2.8, 4.2) } else { (1.4, 2.4) };
            strategy
                .exit("bracket")
                .from(NAME)
                .take(cost + atr * target_atrs)
                .stop(cost - atr * stop_atrs);
            if matches!(self.down.step(fast, slow), Signal::Short) {
                strategy.close(NAME);
            }
        }
    }
}
