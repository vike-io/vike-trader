// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/udt-method-reads-strategy-state-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 1 when EMA(9) crosses up through EMA(21) if the account allows it: flat, 5 bars or more since the bar that asked for the
//! last close, closed result no worse than -5% of the starting cash, and worth above zero. Closed when EMA(9) crosses down. No
//! costs; starts with 1,000,000.
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
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "long";
const CASH: f64 = 1_000_000.0;

struct Risk {
    rest_bars: i64,
    last_close_asked: i64,
    worst_share_lost: f64,
}

impl Risk {
    fn allows_entry(&self, strategy: &Tactics<'_>, bar_index: i64) -> bool {
        strategy.position() == 0.0
            && bar_index - self.last_close_asked >= self.rest_bars
            && strategy.closed_net().unwrap_or(0.0) >= -CASH * self.worst_share_lost
            && strategy.equity() > 0.0
    }
}

struct S {
    risk: Risk,
    fast: IndicatorLine,
    slow: IndicatorLine,
    cross: Cross,
    bar_index: i64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        risk: Risk { rest_bars: 5, last_close_asked: -1000, worst_share_lost: 0.05 },
        fast: IndicatorLine::new("ema", &[9.0]),
        slow: IndicatorLine::new("ema", &[21.0]),
        cross: Cross::new(),
        bar_index: 0,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        let crossed = self.cross.step(fast, slow);
        if matches!(crossed, Signal::Long) && self.risk.allows_entry(strategy, self.bar_index) {
            strategy.entry(NAME, Long).size(1.0);
        }
        if matches!(crossed, Signal::Short) && strategy.position() > 0.0 {
            strategy.close(NAME);
            self.risk.last_close_asked = self.bar_index;
        }
        self.bar_index += 1;
    }
}
