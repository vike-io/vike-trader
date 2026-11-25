// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from the script by zendog123, https://www.tradingview.com/script/9NXf1Bqb/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2021-08-25, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! A long averaging-down bot, its deals started only from 2021-08-01 to 2021-12-31 (bar open times, both included). A deal
//! starts when RSI(7) is at or under 30 with no deal running: a base order of 1,000 worth at the next open. On the close after
//! it fills, one buy limit 3% under the base order's fill price, 700 worth at its level; each time the last one has filled
//! (seen on a close), the next, 3% further under that fill price, up to five. A sell limit for the whole position 2% over its
//! average cost stands from the close the base order has filled on, moved to the new size and cost on every close of the
//! deal. The deal ends at the close of the bar whose high reached that target with the book flat, pulling what still rests.
//! 0.075% commission; up to 99 entries a side; starts with 10,000.
//!
//! The rule's stop loss, its second plan on a timeout and its external indicator are all off by default; the rest only draws.
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
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const BASE: f64 = 1_000.0;
const SAFETY: f64 = 700.0;
const SAFETY_ORDERS: usize = 5;
const DEVIATION: f64 = 3.0;
const TAKE_PROFIT: f64 = 2.0;
/// The first and the last bar open a deal may start on: 2021-08-01 and 2021-12-31, 00:00 UTC.
const DEALS_FROM_MS: i64 = 1_627_776_000_000;
const DEALS_TO_MS: i64 = 1_640_908_800_000;
const NAMES: [&str; SAFETY_ORDERS] = ["safety1", "safety2", "safety3", "safety4", "safety5"];

/// Where a deal stands: none running, its base order asked for, its safety orders being placed.
#[derive(Clone, Copy, PartialEq)]
enum Step {
    Idle,
    BaseAsked,
    SafetyPlaced,
}

struct S {
    rsi: IndicatorLine,
    step: Step,
    placed: usize,
    executed: usize,
    base_price: f64,
    take_price: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        rsi: IndicatorLine::new("rsi", &[7.0]),
        step: Step::Idle,
        placed: 0,
        executed: 0,
        base_price: f64::NAN,
        take_price: f64::NAN,
    })
}

impl S {
    /// Places the next safety order, 3% further under the base order's fill price than the one before.
    fn place_next(&mut self, strategy: &mut Tactics<'_>) {
        self.placed += 1;
        let price = self.base_price * (1.0 - DEVIATION * self.placed as f64 / 100.0);
        let size = strategy.sizing().floor(SAFETY / price);
        strategy.entry(NAMES[self.placed - 1], Long).size(size).limit(price);
        self.step = Step::SafetyPlaced;
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.075), pyramiding: 99, ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let rsi = self.rsi.next(bar)[0];
        let in_range = (DEALS_FROM_MS..=DEALS_TO_MS).contains(&bar.timestamp);
        if in_range && rsi <= 30.0 && self.step == Step::Idle {
            let size = strategy.sizing().floor(BASE / bar.close);
            strategy.entry("base", Long).size(size);
            self.step = Step::BaseAsked;
        }
        let started = strategy.position() > 0.0;
        if started {
            self.take_price = strategy.avg_cost() * (1.0 + TAKE_PROFIT / 100.0);
            let open = strategy.lots().len();
            if open == 1 && self.step == Step::BaseAsked {
                self.base_price = strategy.avg_cost();
                self.place_next(strategy);
            }
            if open > self.placed && self.step == Step::SafetyPlaced {
                if self.placed < SAFETY_ORDERS {
                    self.executed += 1;
                    self.place_next(strategy);
                } else if self.executed < self.placed {
                    self.executed += 1;
                }
            }
            let size = strategy.position().abs();
            strategy.order("target", Short).size(size).limit(self.take_price);
        }
        if bar.high >= self.take_price && strategy.position() == 0.0 {
            strategy.cancel_all();
            self.step = Step::Idle;
            self.placed = 0;
            self.executed = 0;
            self.base_price = f64::NAN;
            self.take_price = f64::NAN;
        }
    }
}
