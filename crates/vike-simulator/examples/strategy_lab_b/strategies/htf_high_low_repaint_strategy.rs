// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "HTF High/Low Repaint Strategy" by Trendoscope, https://www.tradingview.com/script/5FvIVVS2/
// Changed: rewritten by hand in Rust for vike.

//! Quarterly bars (the call's `3M`), each read with lookahead: the current quarter's high and low as the quarter ends, and
//! the previous quarter's. Long all of the account's worth while both the high and the low are above the previous quarter's,
//! short while both are below, each entry reversing the other. 0.01% commission; starts with 20,000.
//!
//! The rule looks ahead: on every bar of a quarter it reads the quarter's FINAL high and low (`lookahead_on`, no `[1]`), the
//! values the quarter has not made yet. The last quarter ends where the reference's data does, at the window's last bar
//! (`DATA_END`), so its high and low are those of the bars up to there. The quarters before the window come from the venue's
//! daily bars, the ones inside it from the chart's own 30-minute bars, which `timeframe::Higher` has no 3-month period for.
//!
//! shortcut: the file takes the venue's daily and 30-minute bars itself (`timeframe::days`, `timeframe::half_hours`), and
//! `DATA_END` is this row's own window end; another row would need its own.

use crate::desk::{Plan, Terms};
use std::collections::BTreeMap;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{RuleDefaultsFee, Tactics};

use crate::timeframe;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_utility::calendar::civil_from_days;

/// The open of the reference's last bar, 2026-09-08 15:00 UTC: where its data, and so the last quarter, ends.
const DATA_END: i64 = 1_788_879_600_000;
/// The window's first bar, 2025-01-01 00:00 UTC: the venue's daily bars serve the quarters before it.
const WINDOW_START: i64 = 1_735_689_600_000;
const DAY_MS: i64 = 86_400_000;
/// The months between January and the month a quarter starts in, for quarters as the calendar has them.
const ANCHOR: i64 = 0;

/// The quarter a bar's open lies in, as a count of quarters.
fn quarter(ts: i64) -> i64 {
    let (y, m, _) = civil_from_days(ts.div_euclid(DAY_MS));
    (y * 12 + (i64::from(m) - 1) - ANCHOR).div_euclid(3)
}

struct S {
    /// `(high, low)` of each quarter as it ends.
    quarters: BTreeMap<i64, (f64, f64)>,
}

pub fn new() -> Box<dyn Plan> {
    let mut quarters: BTreeMap<i64, (f64, f64)> = BTreeMap::new();
    let days = timeframe::days().iter().filter(|b| b.timestamp < WINDOW_START);
    let chart = timeframe::half_hours()
        .0
        .iter()
        .filter(|b| (WINDOW_START..=DATA_END).contains(&b.timestamp));
    for b in days.chain(chart) {
        let q = quarters.entry(quarter(b.timestamp)).or_insert((b.high, b.low));
        *q = (q.0.max(b.high), q.1.min(b.low));
    }
    Box::new(S { quarters })
}

impl Plan for S {
    fn terms(&self) -> Terms {
        Terms { cash: 20_000.0, commission: RuleDefaultsFee::Percent(0.01), ..Terms::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let q = quarter(bar.timestamp);
        let (Some(&(high, low)), Some(&(last_high, last_low))) =
            (self.quarters.get(&q), self.quarters.get(&(q - 1)))
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
