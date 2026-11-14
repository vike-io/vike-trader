// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from the script by zendog123, https://www.tradingview.com/script/Ejzw96lp/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2021-12-16, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! A long averaging-down bot. A deal starts when RSI(7) is at or under 30 with no deal running: a base order of 1,000 worth at
//! the next open. From the close after it fills, five buy limits 3%, 6%, 9%, 12% and 15% under the base order's fill price,
//! 700 worth each at its own level, and a sell limit for the whole position 2% over its average cost, moved to the new size and
//! cost on every close of the deal. The deal ends at the close of the bar that leaves the book flat, pulling what still rests.
//! 0.075% commission; up to 99 entries a side; starts with 10,000.
//!
//! The rule's stop loss, its second plan on a timeout and its external indicator are all off by default.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const BASE: f64 = 1_000.0;
const SAFETY: f64 = 700.0;
const SAFETY_ORDERS: usize = 5;
const DEVIATION: f64 = 3.0;
const TAKE_PROFIT: f64 = 2.0;
const NAMES: [&str; SAFETY_ORDERS] = ["so1", "so2", "so3", "so4", "so5"];

struct S {
    rsi: IndicatorLine,
    in_deal: bool,
    orders_placed: bool,
    take_price: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        rsi: IndicatorLine::new("rsi", &[7.0]),
        in_deal: false,
        orders_placed: false,
        take_price: f64::NAN,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.075), pyramiding: 99, ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let rsi = self.rsi.next(bar)[0];
        let started = strategy.position() > 0.0;
        if rsi <= 30.0 && !started && !self.in_deal {
            self.in_deal = true;
            strategy.entry("bo", Long).size(sizing.floor(BASE / bar.close));
        }
        if started && self.in_deal {
            if strategy.lots().len() == 1 && !self.orders_placed {
                self.orders_placed = true;
                let base = strategy.avg_cost();
                for (k, name) in NAMES.iter().enumerate() {
                    let price = base * (1.0 - DEVIATION * (k + 1) as f64 / 100.0);
                    strategy.entry(name, Long).size(sizing.floor(SAFETY / price)).limit(price);
                }
            }
            self.take_price = strategy.avg_cost() * (1.0 + TAKE_PROFIT / 100.0);
            let size = strategy.position().abs();
            strategy.order("tp", Short).size(size).limit(self.take_price);
        }
        let took = bar.high >= self.take_price && strategy.position() == 0.0;
        if took && !started && self.in_deal {
            strategy.cancel_all();
            self.in_deal = false;
            self.orders_placed = false;
            self.take_price = f64::NAN;
        }
    }
}
