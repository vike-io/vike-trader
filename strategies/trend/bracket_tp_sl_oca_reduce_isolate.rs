// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/bracket-tp-sl-oca-reduce-isolate-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 1 when SMA(9) crosses up through SMA(21) and not already long, short 1 when it crosses down and not already short. While
//! held, on every bar: a free closing limit 10 ticks past and a free closing stop 10 ticks short of the average cost, each the size
//! held, in one group. They stay free, so one left resting by a reversal adds to the new position. Flat, both are pulled.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const UP: &str = "long";
const DOWN: &str = "short";
const TARGET: &str = "target";
const STOP: &str = "stop";
const GROUP: &str = "pair";
const AWAY_TICKS: f64 = 10.0;

struct S {
    fast: IndicatorLine,
    slow: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: IndicatorLine::new("sma", &[9.0]),
        slow: IndicatorLine::new("sma", &[21.0]),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        let signal = self.cross.step(fast, slow);
        let held = strategy.position();
        if matches!(signal, Signal::Long) && held <= 0.0 {
            strategy.entry(UP, Long).size(1.0);
        }
        if matches!(signal, Signal::Short) && held >= 0.0 {
            strategy.entry(DOWN, Short).size(1.0);
        }
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
