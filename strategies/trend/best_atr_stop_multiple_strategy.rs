// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "BEST ATR Stop Multiple Strategy" by Daveatt, https://www.tradingview.com/script/UA83Q4Je/
// Changed: rewritten by hand in Rust for vike.

//! SMA(15) against SMA(45) of the close: long 100 when the fast crosses up through the slow, short 100 when it crosses down,
//! each reversing the other. On the signal bar the rule keeps the close and twice ATR(14). While the last cross was up, the
//! long is closed when the low crosses down through the kept close less the kept ATRs as it stood the bar before; while it
//! was down, the short is closed when the high crosses up through the kept close plus them. 0.075% commission; starts with
//! 100,000.
//!
//! Neither trend holds until both a cross up and a cross down have happened (a comparison with a missing bars-since count is
//! false); the level a trend does not hold is 0 under a long and 999,999 over a short, so it can be crossed only from a bar
//! on which it was the other.
//!
//! The header recalculates on order fills (`Rule::after_fill`): after a fill inside a bar the rule runs again on the WHOLE
//! bar, its persistent values rolled back to the bar's start, so a signal the bar's own close would give is acted on at that
//! fill's price, in the same bar. Only bars that open with a fill have such a pass; the rule's definitive run follows on the
//! bar's close as on any other.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Atr, Sma};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::helpers::fewer;
use vike_tactics::{RuleDefaults, Tactics};

const SIZE: f64 = 100.0;
const LONG_NAME: &str = "BEST L";
const SHORT_NAME: &str = "BEST S";

/// The orders one run of the rule gives.
struct Orders {
    long: bool,
    short: bool,
    close_long: bool,
    close_short: bool,
}

#[derive(Clone)]
struct Rule {
    fast: Sma,
    slow: Sma,
    last_fast: f64,
    last_slow: f64,
    atr: Atr,
    /// Bars since the last cross up and down, `None` before the first.
    since_buy: Option<usize>,
    since_sell: Option<usize>,
    entry_price: f64,
    entry_atr: f64,
    /// The stop levels of the bar before and the one before that, and the low and high of the bar before.
    long_stop: [f64; 2],
    short_stop: [f64; 2],
    last_low: f64,
    last_high: f64,
}

impl Rule {
    fn new() -> Self {
        Rule {
            fast: Sma::new(15.0).expect("a valid length"),
            slow: Sma::new(45.0).expect("a valid length"),
            last_fast: f64::NAN,
            last_slow: f64::NAN,
            atr: Atr::new(14.0).expect("a valid length"),
            since_buy: None,
            since_sell: None,
            entry_price: f64::NAN,
            entry_atr: 0.0,
            long_stop: [f64::NAN; 2],
            short_stop: [f64::NAN; 2],
            last_low: f64::NAN,
            last_high: f64::NAN,
        }
    }

    /// One run of the rule on `bar`, which the rule's values then keep.
    fn step(&mut self, bar: &Bar) -> Orders {
        let fast = self.fast.next_value(bar.close);
        let slow = self.slow.next_value(bar.close);
        let long = fast > slow && self.last_fast <= self.last_slow;
        let short = fast < slow && self.last_fast >= self.last_slow;
        (self.last_fast, self.last_slow) = (fast, slow);
        self.since_buy = since(self.since_buy, long);
        self.since_sell = since(self.since_sell, short);
        let buy_trend = fewer(self.since_buy, self.since_sell);
        let sell_trend = fewer(self.since_sell, self.since_buy);
        if long || short {
            self.entry_price = bar.close;
        }
        let atr = self.atr.next(bar);
        if long || short {
            self.entry_atr = if atr.is_nan() { 0.0 } else { atr } * 2.0;
        }
        let long_stop = if buy_trend { self.entry_price - self.entry_atr } else { 0.0 };
        let short_stop = if sell_trend { self.entry_price + self.entry_atr } else { 999_999.0 };
        // A cross of the low under the stop as it stood the bar before: under it now, at or over it the bar before.
        let [stop_1, stop_2] = self.long_stop;
        let close_long = buy_trend && bar.low < stop_1 && self.last_low >= stop_2;
        let [stop_1, stop_2] = self.short_stop;
        let close_short = sell_trend && bar.high > stop_1 && self.last_high <= stop_2;
        self.long_stop = [long_stop, self.long_stop[0]];
        self.short_stop = [short_stop, self.short_stop[0]];
        (self.last_low, self.last_high) = (bar.low, bar.high);
        Orders { long, short, close_long, close_short }
    }
}

/// The reference's bars-since: 0 on a bar the condition holds, one more on each bar after. `helpers::Since` is not `Clone`.
fn since(count: Option<usize>, now: bool) -> Option<usize> {
    if now { Some(0) } else { count.map(|n| n + 1) }
}

impl Orders {
    /// The calls in the rule's order: each entry, then the close of its side.
    fn place(&self, strategy: &mut Tactics<'_>) {
        if self.long {
            strategy.entry(LONG_NAME, Long).size(SIZE);
        }
        if self.close_long {
            strategy.close(LONG_NAME);
        }
        if self.short {
            strategy.entry(SHORT_NAME, Short).size(SIZE);
        }
        if self.close_short {
            strategy.close(SHORT_NAME);
        }
    }
}

struct S {
    rule: Rule,
}

pub fn rule(_params: &Params) -> Box<dyn vike_tactics::Rule + Send> {
    Box::new(S { rule: Rule::new() })
}

impl vike_tactics::Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.075), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        self.rule.step(bar).place(strategy);
    }

    fn after_fill(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        // A pass whose values are thrown away: the rule's state is what the bar started with.
        self.rule.clone().step(bar).place(strategy);
    }
}
