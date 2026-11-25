// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Combo Backtest 123 Reversal & Smart Money Index (SMI)" by HPotter, https://www.tradingview.com/script/NudssmJx/
// The original's notice: Copyright by HPotter v2.0 19/02/2023
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2021-07-09, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Long 1 while the 123 reversal and a smart money index both say long, short 1 while both say short, each reversing the
//! other, and flat otherwise. No costs; starts with 100,000.
//!
//! The reversal reads the stochastic of the bar before over the 14 bars before (its %K, unsmoothed, against its 3-bar SMA):
//! long after two closes falling then rising with %K under its average and above 50, short on the mirror with %K above its
//! average and under 50, holding the last side between. The index starts at 1 (and again after a missing value) and adds each
//! bar's close less its open and the daily open less the daily close of the day before the last closed day; long while its
//! 18-bar SMA is above it, short while under, holding the last side between. The days are the run's own symbol's daily
//! bars, each read once it has closed (`vike_tactics::higher::HigherTimeframeSourceCalendar`).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Stochastic;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::higher::{self, HigherTimeframeSourceCalendar};
use vike_tactics::rolling::RollingSum;
use vike_tactics::{HigherTimeframePeriod, Rule, RuleDefaults, Tactics};

struct S {
    days: HigherTimeframeSourceCalendar,
    /// `(open, close)` of the last closed day and of the one before it.
    last_day: (f64, f64),
    day_before: (f64, f64),
    /// The raw %K and its SMA.
    stoch: Stochastic,
    /// Those of the bar before, waiting a bar to be read.
    before: [f64; 2],
    /// The last three closes, newest first.
    closes: [f64; 3],
    reversal: i32,
    index: f64,
    index_sum: RollingSum,
    smart: i32,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        days: HigherTimeframeSourceCalendar::new(HigherTimeframePeriod::Day),
        last_day: (f64::NAN, f64::NAN),
        day_before: (f64::NAN, f64::NAN),
        stoch: Stochastic::new(14.0, 1.0, 3.0).expect("valid lengths").with_fresh_sum(true),
        before: [f64::NAN; 2],
        closes: [f64::NAN; 3],
        reversal: 0,
        index: f64::NAN,
        index_sum: RollingSum::new(18),
        smart: 0,
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
        for day in self.days.next(bar, strategy.input(higher::DAY)) {
            self.day_before = self.last_day;
            self.last_day = (day.open, day.close);
        }
        let [fast, slow] = self.before;
        let [c1, c2, c3] = self.closes;
        if c3 < c2 && c1 > c2 && fast < slow && fast > 50.0 {
            self.reversal = 1;
        } else if c3 > c2 && c1 < c2 && fast > slow && fast < 50.0 {
            self.reversal = -1;
        }
        self.before = self.stoch.next(bar);
        self.closes = [bar.close, c1, c2];

        let (open, close) = self.day_before;
        let prior = if self.index.is_nan() { 1.0 } else { self.index };
        self.index = prior - (bar.open - bar.close) + (open - close);
        let average = self.index_sum.next(self.index) / 18.0;
        if average > self.index {
            self.smart = 1;
        } else if average < self.index {
            self.smart = -1;
        }
        match (self.reversal, self.smart) {
            (1, 1) => {
                strategy.entry("Long", Long).size(1.0);
            }
            (-1, -1) => {
                strategy.entry("Short", Short).size(1.0);
            }
            _ => {
                strategy.close_all();
            }
        }
    }
}
