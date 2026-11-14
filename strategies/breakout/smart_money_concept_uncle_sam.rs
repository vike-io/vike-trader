// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Smart Money Concept - Uncle Sam" by UncleSamTrading, https://www.tradingview.com/script/KUeL7SY5/
// The original's notice: © Kostinay
// Changed: rewritten by hand in Rust for vike.

//! The break-of-structure rule of `smart_money_pivot_strategy_jason_kasei` (swings of 20 bars, the 1% stop and 1.5% target,
//! every order at the bar's close), on an account of 1,000. No costs.
//!
//! Its moving-average filter is off, its size caps never bind, and its volume ratios and labels are drawings only.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::ConfirmedPivot;
use vike_marketdata::Bar;
use vike_model::OrderSide;
use vike_model::OrderSide::Long;
use vike_model::OrderSide::Short;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::RuleDefaults;
use vike_tactics::Tactics;
use vike_tactics::sizing::share_of_worth;

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(Breaks::new())
}

const UP: &str = "Long";
const DOWN: &str = "Short";
const SPAN: f64 = 20.0;

/// The swing a break is judged against: the newest pivot, armed while not yet broken.
struct Swing {
    level: f64,
    /// The newest pivot as the rule's `fixnan` holds it.
    newest: f64,
    armed: bool,
}

impl Swing {
    fn new() -> Self {
        Swing { level: f64::NAN, newest: f64::NAN, armed: false }
    }

    /// `newest` is the newest confirmed pivot; a change from the last one re-arms. The first pivot's change is from nothing,
    /// and the rule's comparison of a missing value is false, so a swing first arms on the second pivot at another price.
    fn update(&mut self, newest: f64) {
        let d = newest - self.newest;
        if d != 0.0 && !d.is_nan() {
            self.level = newest;
            self.armed = true;
        }
        self.newest = newest;
    }

    /// `true` once when `broken` holds while armed.
    fn breaks(&mut self, broken: bool) -> bool {
        let fired = broken && self.armed;
        self.armed &= !fired;
        fired
    }
}

/// The rule, on any account size.
struct Breaks {
    pivots: ConfirmedPivot,
    high: Swing,
    low: Swing,
}

impl Breaks {
    fn new() -> Self {
        Breaks {
            pivots: ConfirmedPivot::new(SPAN, SPAN).expect("valid spans"),
            high: Swing::new(),
            low: Swing::new(),
        }
    }

    fn enter(strategy: &mut Tactics<'_>, bar: &Bar, side: OrderSide) {
        let Some(tick) = strategy.tick() else { return };
        let sizing = strategy.sizing();
        let name = if side == Long { UP } else { DOWN };
        let held_here = strategy.position() * f64::from(side) > 0.0;
        strategy.entry(name, side).size(share_of_worth(&sizing, 100.0, bar.close));
        // A new entry fills at this close; one already held keeps its price.
        let base = if held_here { strategy.avg_cost() } else { bar.close };
        let s = f64::from(side);
        let cut = base - s * (bar.close * (1.0 / 100.0) / tick) * tick;
        let take = base + s * (bar.close * (1.5 / 100.0) / tick) * tick;
        strategy
            .exit(if side == Long { "Long exit" } else { "Short exit" })
            .from(name)
            .take(take)
            .stop(cut);
    }
}

impl Rule for Breaks {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let [_, _, newest_high, newest_low] = self.pivots.next(bar);
        self.high.update(newest_high);
        self.low.update(newest_low);
        let buy = self.high.breaks(bar.high > self.high.level);
        let sell = self.low.breaks(bar.low < self.low.level);
        if buy {
            Self::enter(strategy, bar, Long);
        }
        if sell {
            Self::enter(strategy, bar, Short);
        }
    }
}
