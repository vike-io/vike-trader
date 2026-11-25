// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "RSI + MACD Multi-Timeframe Strategy" by RWCS_LTD, https://www.tradingview.com/script/Epqb0L8C/
// The original's notice: © RWCS_LTD
// Changed: rewritten by hand in Rust for vike.

//! Long all of the account's worth when the daily RSI(14) is under 30 and the 4-hour MACD(12, 26, 9) crosses up through its
//! signal; closed when the RSI is over 70 and the MACD crosses down. Both are the last closed bar's, and the cross is judged on
//! the 30-minute bars, between the values they see. No costs; version 6 with no size or capital: 100,000 at 100% of equity,
//! margined at 100%.
//!
//! The days are the symbol's daily input, its history before the window included (`vike_tactics::higher::HigherTimeframeSourceCalendar`); the 4-hour bars
//! are built from its 30-minute input from the rule's first bar (`vike_tactics::higher::HigherTimeframeSourceIntraday`).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Macd, Rsi};
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::higher::{self, HigherTimeframeSourceCalendar, HigherTimeframeSourceIntraday};
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{HigherTimeframePeriod, Rule, RuleDefaults, Tactics};

struct S {
    days: HigherTimeframeSourceCalendar,
    fours: HigherTimeframeSourceIntraday,
    rsi: Rsi,
    macd: Macd,
    /// The last closed day's RSI, and the last closed 4-hour bar's MACD line and signal.
    strength: f64,
    lines: (f64, f64),
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        days: HigherTimeframeSourceCalendar::new(HigherTimeframePeriod::Day),
        fours: HigherTimeframeSourceIntraday::new(240),
        rsi: Rsi::new(14.0).expect("a valid length"),
        macd: Macd::new(12.0, 26.0, 9.0).expect("valid lengths"),
        strength: f64::NAN,
        lines: (f64::NAN, f64::NAN),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn inputs(&self) -> Vec<String> {
        vec![higher::DAY.to_string(), higher::HALF_HOUR.to_string()]
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        for day in self.days.next(bar, strategy.input(higher::DAY)) {
            self.strength = self.rsi.next(&day);
        }
        for four in self.fours.next(bar, strategy.input(higher::HALF_HOUR)) {
            let [line, signal, _] = self.macd.next(&four);
            self.lines = (line, signal);
        }
        let cross = self.cross.step(self.lines.0, self.lines.1);
        if self.strength < 30.0 && matches!(cross, Signal::Long) {
            strategy.entry("Long", Long).size(share_of_worth(&sizing, 100.0, bar.close));
        }
        if self.strength > 70.0 && matches!(cross, Signal::Short) {
            strategy.close("Long");
        }
    }
}
