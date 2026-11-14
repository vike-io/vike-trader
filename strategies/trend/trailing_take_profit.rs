// SPDX-License-Identifier: MIT
// Copyright © 2022-present Iason Nikolas | jason5480. Licensed under the MIT license; its text, with this notice, is LICENSE-MIT in the strategies folder.
// Derived from "Trailing Take Profit" by jason5480, https://www.tradingview.com/script/W8nc1c3V/
// Changed: rewritten by hand in Rust for vike.

//! Long all of the account's worth at the next open when SMA(21) of the close crosses up through SMA(49) with no trade open.
//! Held, it carries one trailing exit stated from the signal bar: it arms at 7% over the signal bar's close, rounded down to
//! the tick, then follows the best price by 1% of that level, cut to whole ticks. There is no stop and no other exit. No
//! costs; starts with 100,000.
//!
//! The rule reads its date window from the imported library `chrono_utils` 7 (by jason5480, MPL-2.0); with no date set the
//! window includes every bar, so nothing of it is ported. The reference reads the size of "the last open trade" as 0 while
//! flat, which makes the entry valid then. The exit is issued once, when the entry fills: the rule restates it unchanged
//! every bar, which keeps the armed state.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Sma;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics, TacticsTrail};

const PROFIT: f64 = 1.0 + 7.0 / 100.0;
const DISTANCE: f64 = 1.0 / 100.0;

struct S {
    fast: Sma,
    slow: Sma,
    cross: Cross,
    /// The arming level stated on the signal bar.
    level: f64,
    armed: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: Sma::new(21.0).expect("a valid length"),
        slow: Sma::new(49.0).expect("a valid length"),
        cross: Cross::new(),
        level: f64::NAN,
        armed: false,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        if strategy.tick().is_none() {
            return; // its take-profit trail counts in ticks
        }
        let sizing = strategy.sizing();
        let crossed =
            matches!(self.cross.step(self.fast.next(bar), self.slow.next(bar)), Signal::Long);
        if crossed && strategy.position() == 0.0 {
            self.level = bar.close * PROFIT;
            strategy.entry("Long", Long).size(share_of_worth(&sizing, 100.0, bar.close));
        }
    }

    fn after_fill(&mut self, strategy: &mut Tactics<'_>, _bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        if strategy.position() <= 0.0 {
            self.armed = false;
        } else if !self.armed {
            self.armed = true;
            // The arming price rounds toward the position (down for a long, within 1e-4 of a tick), the offset truncates.
            let arm = (self.level / tick + 1e-4).floor();
            let offset = (self.level * DISTANCE / tick).trunc();
            let after = arm - (strategy.avg_cost() / tick).round();
            let trail = TacticsTrail { after_ticks: after, by_ticks: offset };
            strategy.exit("Long Take Profit").from("Long").trail(trail);
        }
    }
}
