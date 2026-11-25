// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "HTF High/Low Repaint Strategy" by Trendoscope, https://www.tradingview.com/script/5FvIVVS2/
// The original's notice: © HeWhoMustNotBeNamed
// Changed: rewritten by hand in Rust for vike.

//! Quarterly bars (the call's `3M`), each read with lookahead: the current quarter's high and low as the quarter ends, and
//! the previous quarter's. Long all of the account's worth while both the high and the low are above the previous quarter's,
//! short while both are below, each entry reversing the other. 0.01% commission; starts with 20,000.
//!
//! **It looks into the future.** On every bar of a quarter the rule reads the quarter's FINAL high and low (`lookahead_on`,
//! no `[1]`), the values the quarter has not made yet, which no live run can know. So it runs in a backtest only, and its
//! backtest is better than reality. The quarters come from the run's whole series (`vike_tactics::Tactics::input_whole`):
//! the ones before the rule's first bar from the daily bars, the ones from it on from the 30-minute bars, which end where the
//! run does, so the last quarter's high and low are those of the bars up to the run's last bar, where the reference's data
//! ended too.
//!
//! ```toml
//! [strategy]
//! runs_on = "backtest"
//! why = "looks into the future: reads the quarter in progress at its final values; a backtest is better than reality"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::BTreeMap;

use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::higher::{DAY, HALF_HOUR};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{HigherTimeframePeriod, Rule, RuleDefaults, Tactics};

const QUARTER: HigherTimeframePeriod = HigherTimeframePeriod::Quarter;

struct S {
    /// `(high, low)` of each quarter as it ends, by the quarter's start: read on the first bar.
    quarters: Option<BTreeMap<i64, (f64, f64)>>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S { quarters: None })
}

/// Every quarter's final high and low: the whole daily series before `first` (the rule's first bar) and the whole 30-minute
/// series from it on.
fn quarters_of(first: i64, strategy: &Tactics<'_>) -> BTreeMap<i64, (f64, f64)> {
    let whole = |name: &'static str| {
        strategy.input_whole(name).unwrap_or_else(|| {
            panic!("the quarters need the whole series, which a backtest serves a runs_on = \"backtest\" file alone")
        })
    };
    let days = whole(DAY).iter().filter(|b| b.timestamp < first);
    let half_hours = whole(HALF_HOUR).iter().filter(|b| b.timestamp >= first);
    let mut quarters: BTreeMap<i64, (f64, f64)> = BTreeMap::new();
    for b in days.chain(half_hours) {
        let q = quarters.entry(QUARTER.start(b.timestamp)).or_insert((b.high, b.low));
        *q = (q.0.max(b.high), q.1.min(b.low));
    }
    quarters
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.01), ..RuleDefaults::PLAIN }
    }

    fn inputs(&self) -> Vec<String> {
        vec![DAY.to_string(), HALF_HOUR.to_string()]
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let quarters = self.quarters.get_or_insert_with(|| quarters_of(bar.timestamp, strategy));
        let q = QUARTER.start(bar.timestamp);
        let (Some(&(high, low)), Some(&(last_high, last_low))) =
            (quarters.get(&q), quarters.get(&QUARTER.start(q - 1)))
        else {
            return;
        };
        if high > last_high && low > last_low {
            strategy.entry("Buy", Long).size(share_of_worth(&sizing, 100.0, bar.close));
        }
        if high < last_high && low < last_low {
            strategy.entry("Sell", Short).size(share_of_worth(&sizing, 100.0, bar.close));
        }
    }
}
