// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "RSI Crossover Strategy with Compounding (Monthly)" by Anmol-Mittal, https://www.tradingview.com/script/rHDrnos6/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2024-10-04, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! The monthly RSI(14) of the close against the monthly SMA(14) of that RSI, both as of the last month closed. Flat with the
//! RSI above its average, long as many whole BTC as the rule's own capital buys at this close; closed with the RSI below it,
//! when that capital grows by the account's whole net profit so far (the rule adds it each time, not the trade's: the run's
//! closed trades, `vike_tactics::Tactics::closed_net`). The rule's capital starts at 100,000; the account, version 5 with none
//! stated, at 1,000,000. No costs.
//!
//! The months are built from the symbol's daily input, its history before the window included (`vike_tactics::higher::HigherTimeframeSourceCalendar`).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Rsi;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::higher::{self, HigherTimeframeSourceCalendar};
use vike_tactics::rolling::RollingSum;
use vike_tactics::{HigherTimeframePeriod, Rule, RuleDefaults, Tactics};

const LENGTH: usize = 14;

struct S {
    months: HigherTimeframeSourceCalendar,
    rsi: Rsi,
    sum: RollingSum,
    /// The last closed month's RSI and its average.
    line: (f64, f64),
    capital: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        months: HigherTimeframeSourceCalendar::new(HigherTimeframePeriod::Month),
        rsi: Rsi::new(14.0).expect("a valid length"),
        sum: RollingSum::new(LENGTH),
        line: (f64::NAN, f64::NAN),
        capital: 100_000.0,
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
        for month in self.months.next(bar, strategy.input(higher::DAY)) {
            let rsi = self.rsi.next(&month);
            self.line = (rsi, self.sum.next(rsi) / LENGTH as f64);
        }
        let (rsi, average) = self.line;
        let most = if self.capital > 0.0 { (self.capital / bar.close).floor() } else { 0.0 };
        if rsi > average && strategy.position() == 0.0 && most > 0.0 {
            strategy.entry("Long", Long).size(most);
        }
        if rsi < average && strategy.position() > 0.0 {
            strategy.close("Long");
            self.capital += strategy.closed_net().unwrap_or(0.0);
        }
    }
}
