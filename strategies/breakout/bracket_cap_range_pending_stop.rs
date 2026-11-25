// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-bracket-cap-range-pending-stop-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! When the previous bar's range passes the ATR(14) before it, a stop entry past that bar: over its high when it closed up, under its
//! low when it closed down. Otherwise that side's entry is pulled. While held, on every bar: a free closing limit and a free closing
//! stop 10 ticks either side of the average cost, each the size held, in one group; flat, both are pulled. Five fills a UTC day.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! fills_a_day = 5
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const UP: &str = "long";
const DOWN: &str = "short";
const TARGET: &str = "target";
const STOP: &str = "stop";
const GROUP: &str = "pair";
const AWAY_TICKS: f64 = 10.0;

struct S {
    atr: IndicatorLine,
    previous_bar: Option<Bar>,
    previous_atr: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        atr: IndicatorLine::new("atr", &[14.0]),
        previous_bar: None,
        previous_atr: f64::NAN,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let atr = self.atr.next(bar)[0];
        let previous_atr = std::mem::replace(&mut self.previous_atr, atr);
        let Some(previous) = self.previous_bar.replace(bar.clone()) else { return };
        let expanded = previous.high - previous.low > previous_atr;
        if expanded && previous.close > previous.open {
            strategy.entry(UP, Long).size(1.0).stop(previous.high);
        } else {
            strategy.cancel(UP);
        }
        if expanded && previous.close < previous.open {
            strategy.entry(DOWN, Short).size(1.0).stop(previous.low);
        } else {
            strategy.cancel(DOWN);
        }
        let held = strategy.position();
        if held == 0.0 {
            strategy.cancel(TARGET);
            strategy.cancel(STOP);
            return;
        }
        let (toward, closing) = if held > 0.0 { (1.0, Short) } else { (-1.0, Long) };
        let away = toward * AWAY_TICKS * tick;
        let cost = strategy.avg_cost();
        strategy.order(TARGET, closing).size(held.abs()).limit(cost + away).group(GROUP);
        strategy.order(STOP, closing).size(held.abs()).stop(cost - away).group(GROUP);
    }
}
