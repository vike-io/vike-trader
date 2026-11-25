// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Backtest Template [Backtest Terminal]" by BacktestTerminal, https://www.tradingview.com/script/KQpraXem/
// The original's notice: © BacktestTerminal
// Changed: rewritten by hand in Rust for vike.

//! Long 1 when EMA(9) crosses up through EMA(21) with the close above EMA(200), short 1 when it crosses down with the close
//! below, each only when flat at this close and at the last. While held, a stop 1.00 and a target 2.00 from the close of the
//! bar that last opened a trade, issued once the position is held, the stop filling first at an open past both. 0.07 a
//! contract each fill, 2 ticks of slippage; starts with 10,000.
//!
//! The levels are the rule's own variables: a signal on a bar that was flat at the last close sets them even when the
//! position filled this bar, so such a bar moves a held trade's exit to the other side's levels. The rule's date range,
//! sessions and trading days hold on every bar here.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Ema;
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const STOP_POINTS: f64 = 100.0;
const TARGET_POINTS: f64 = 200.0;

struct S {
    fast: Ema,
    slow: Ema,
    trend: Ema,
    cross: Cross,
    /// The position at the last close.
    was: f64,
    stop: f64,
    target: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let ema = |n: f64| Ema::new(n).expect("a valid length");
    Box::new(S {
        fast: ema(9.0),
        slow: ema(21.0),
        trend: ema(200.0),
        cross: Cross::new(),
        was: 0.0,
        stop: 0.0,
        target: 0.0,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::per_contract(0.07), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let close = bar.close;
        let fast = self.fast.next(bar);
        let slow = self.slow.next(bar);
        let trend = self.trend.next(bar);
        let cross = self.cross.step(fast, slow);
        let flat_before = self.was == 0.0;
        let long = matches!(cross, Signal::Long) && close > trend && flat_before;
        let short = matches!(cross, Signal::Short) && close < trend && flat_before;
        let divider = 1.0 / tick;
        let (stop_by, target_by) = (STOP_POINTS / divider, TARGET_POINTS / divider);
        let long_levels = (close - stop_by, close + target_by);
        let short_levels = (close + stop_by, close - target_by);
        if long || short {
            (self.stop, self.target) = if long { long_levels } else { short_levels };
        }
        let held = strategy.position();
        if held == 0.0 {
            if long {
                strategy.entry("LONG", Long).size(1.0);
            } else if short {
                strategy.entry("SHORT", Short).size(1.0);
            }
        }
        let lots = if held > 0.0 { "LONG" } else { "SHORT" };
        if held != 0.0 {
            // The stop is queued ahead of the target, each pulling the other: on a bar that opens past both, the stop fills.
            strategy.exit("exit").from(lots).take(self.target).stop(self.stop);
        }
        self.was = held;
    }
}
