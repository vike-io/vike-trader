// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Camarilla Pivot Points V2 Backtest" by HPotter, https://www.tradingview.com/script/MXkclJVM/
// The original's notice: Copyright by HPotter v1.0 12/05/2020
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2020-05-12, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Camarilla levels of the last closed day: its close plus and minus 0.0916 of its range. Long 1 while the close is above the
//! lower level, else short 1 while it is under the upper one, holding the last side between, each reversing the other; flat
//! before either. No costs; starts with 100,000.
//!
//! The day is the run's own symbol's daily bar, read once it has closed (`vike_tactics::higher::HigherTimeframeSourceCalendar`).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::higher::{self, HigherTimeframeSourceCalendar};
use vike_tactics::{HigherTimeframePeriod, Rule, RuleDefaults, Tactics};

const SPREAD: f64 = 0.0916;

struct S {
    days: HigherTimeframeSourceCalendar,
    /// The last closed day's `(high, low, close)`.
    day: (f64, f64, f64),
    side: i32,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        days: HigherTimeframeSourceCalendar::new(HigherTimeframePeriod::Day),
        day: (f64::NAN, f64::NAN, f64::NAN),
        side: 0,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn inputs(&self) -> Vec<String> {
        vec![higher::DAY.to_string()]
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        if let Some(day) = self.days.next(bar, strategy.input(higher::DAY)).last() {
            self.day = (day.high, day.low, day.close);
        }
        let (high, low, close) = self.day;
        // Not a `PivotPoints` option: the rule's spread 0.0916 is not Camarilla's 1.1 / 12, and it reads one level of eight.
        let upper = SPREAD * (high - low) + close;
        let lower = close - SPREAD * (high - low);
        if bar.close > lower {
            self.side = 1;
        } else if bar.close < upper {
            self.side = -1;
        }
        match self.side {
            1 => {
                strategy.entry("Long", Long).size(1.0);
            }
            -1 => {
                strategy.entry("Short", Short).size(1.0);
            }
            _ => {
                strategy.close_all();
            }
        }
    }
}
