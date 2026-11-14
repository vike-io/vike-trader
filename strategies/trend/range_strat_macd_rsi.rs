// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Range Strat - MACD/RSI" by kuhoerk, https://www.tradingview.com/script/XWgu79vU/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2022-08-22, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Long 10% of the account's worth when the MACD histogram (12, 26, 9, of the bar's open) crosses up through 0 with RSI(14) of the
//! open at or under 55, short 10% when it crosses down with RSI at or over 50, each reversing the other and at most two entries
//! a side. Only on the bars from 2022-06-13 to (not including) 2024-06-13. A stop 15% from the position's average price guards
//! each side, asked again on every bar. 0.05% commission; starts with 1,000.
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
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::calendar::timestamp;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const STOP: f64 = 0.15;

struct S {
    macd: Macd,
    rsi: Rsi,
    cross: Cross,
    from: i64,
    to: i64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        macd: Macd::new(12.0, 26.0, 9.0).expect("valid lengths"),
        rsi: Rsi::new(14.0).expect("a valid length").with_rs_form(true),
        cross: Cross::new(),
        from: timestamp(2022, 6, 13, 0, 0),
        to: timestamp(2024, 6, 13, 0, 0),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.05), pyramiding: 2, ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let [_, _, delta] = self.macd.next_value(bar.open);
        let rsi = self.rsi.next_value(bar.open);
        let signal = self.cross.step(delta, 0.0);
        let held = strategy.position();
        let average = strategy.avg_cost();
        if !rsi.is_nan() && bar.timestamp >= self.from && bar.timestamp < self.to {
            let size = share_of_worth(&sizing, 10.0, bar.close);
            match signal {
                Signal::Long if rsi <= 55.0 => {
                    strategy.entry("LONG", Long).size(size);
                }
                Signal::Short if rsi >= 50.0 => {
                    strategy.entry("SHORT", Short).size(size);
                }
                _ => {}
            }
        }
        if held > 0.0 {
            strategy.exit("LONG STOP").from("LONG").stop(average * (1.0 - STOP));
        }
        if held < 0.0 {
            strategy.exit("SHORT STOP").from("SHORT").stop(average * (1.0 + STOP));
        }
    }
}
