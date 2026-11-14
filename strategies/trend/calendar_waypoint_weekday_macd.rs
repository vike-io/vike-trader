// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/calendar-waypoint-weekday-macd-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! On UTC weekdays, long 6% of the account's worth when the MACD(12,26,9) line crosses up through its signal, short 6% when it
//! crosses down, reversing what is held. Closed on the first bar of the weekend. While held, a stop 2.1 ATR(14) from the average
//! cost. 0.05% commission, 1 tick.
//!
//! The cross is only asked on weekdays, so that is all it remembers.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_model::time::utc_weekday;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const UP: &str = "long";
const DOWN: &str = "short";
const SHARE: f64 = 0.06;
const STOP_ATRS: f64 = 2.1;

struct S {
    macd: IndicatorLine,
    atr: IndicatorLine,
    cross: Cross,
    was_allowed: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        macd: IndicatorLine::new("macd", &[12.0, 26.0, 9.0]),
        atr: IndicatorLine::new("atr", &[14.0]),
        cross: Cross::new(),
        was_allowed: false,
    })
}

fn is_weekday(bar: &Bar) -> bool {
    utc_weekday(bar.timestamp) < 5
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let lines = self.macd.next(bar);
        let atr = self.atr.next(bar)[0];
        let allowed = is_weekday(bar);
        let was_allowed = std::mem::replace(&mut self.was_allowed, allowed);
        if allowed {
            match self.cross.step(lines[0], lines[1]) {
                Signal::Long => {
                    let size = sizing.size_for_share(SHARE, sizing.slipped(Long, bar.close));
                    strategy.entry(UP, Long).size(size);
                }
                Signal::Short => {
                    let size = sizing.size_for_share(SHARE, sizing.slipped(Short, bar.close));
                    strategy.entry(DOWN, Short).size(size);
                }
                _ => {}
            }
        } else if was_allowed {
            strategy.close(UP);
            strategy.close(DOWN);
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
