// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Ichimoku + Daily-Candle_X + HULL-MA_X + MacD" by SeaSide420, https://www.tradingview.com/script/RJBjyl2W/
// The original's notice: © SeaSide420
// Changed: rewritten by hand in Rust for vike.

//! Long all of the account's worth when HMA(14) of the open is above HMA(14) of the open before, the last closed day opened
//! above the day before it, the open is above that second average, the cloud's first leading line is above its second and a
//! MACD of Hull averages (HMA(12) less HMA(26) of the open) is above its HMA(9); short on the mirror, each reversing the
//! other. 0.25% commission and 1 tick of slippage; starts with 10,000.
//!
//! The cloud's lines are the midpoints of the highest high and lowest low over 9 and 26 bars, averaged, against 52 bars, read
//! on this bar with no displacement. The days are the run's own symbol's daily bars, each read once it has closed
//! (`vike_tactics::higher::HigherTimeframeSourceCalendar`).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Donchian, Hma};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::higher::{self, HigherTimeframeSourceCalendar};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{HigherTimeframePeriod, Rule, RuleDefaults, RuleDefaultsFee, Tactics};

struct S {
    days: HigherTimeframeSourceCalendar,
    /// The opens of the last closed day and of the day before it.
    day_open: f64,
    day_before: f64,
    hull: Hma,
    hull_before: Hma,
    last_open: f64,
    fast: Hma,
    slow: Hma,
    signal: Hma,
    conversion: Donchian,
    base: Donchian,
    second: Donchian,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let hma = |length: f64| Hma::new(length).expect("a valid length");
    let donchian = |length: f64| Donchian::new(length).expect("a valid length");
    Box::new(S {
        days: HigherTimeframeSourceCalendar::new(HigherTimeframePeriod::Day),
        day_open: f64::NAN,
        day_before: f64::NAN,
        hull: hma(14.0),
        hull_before: hma(14.0),
        last_open: f64::NAN,
        fast: hma(12.0),
        slow: hma(26.0),
        signal: hma(9.0),
        conversion: donchian(9.0),
        base: donchian(26.0),
        second: donchian(52.0),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.25), ..RuleDefaults::PLAIN }
    }

    fn inputs(&self) -> Vec<String> {
        vec![higher::DAY.to_string()]
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        for day in self.days.next(bar, strategy.input(higher::DAY)) {
            self.day_before = self.day_open;
            self.day_open = day.open;
        }
        let price = bar.open;
        let hull = self.hull.next_value(price);
        // The average of the open before starts with the second bar, as its windows hold no missing value from there.
        let before = if self.last_open.is_nan() {
            f64::NAN
        } else {
            self.hull_before.next_value(self.last_open)
        };
        self.last_open = price;
        let macd = self.fast.next_value(price) - self.slow.next_value(price);
        let signal = if macd.is_nan() { f64::NAN } else { self.signal.next_value(macd) };
        let lead1 = (self.conversion.next(bar)[1] + self.base.next(bar)[1]) / 2.0;
        let lead2 = self.second.next(bar)[1];
        let (day, day_before) = (self.day_open, self.day_before);
        if hull > before && day > day_before && price > before && lead1 > lead2 && macd > signal {
            strategy.entry("Long", Long).size(share_of_worth(
                &sizing,
                100.0,
                sizing.slipped(Long, bar.close),
            ));
        }
        if hull < before && day < day_before && price < before && lead1 < lead2 && macd < signal {
            strategy.entry("Short", Short).size(share_of_worth(
                &sizing,
                100.0,
                sizing.slipped(Short, bar.close),
            ));
        }
    }
}
