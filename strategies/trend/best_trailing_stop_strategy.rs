// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "BEST Trailing Stop Strategy" by Daveatt, https://www.tradingview.com/script/NOGqbIcu/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2019-12-12, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Long 100 when SMA(15) of the close crosses up through SMA(45), short 100 when it crosses down, each reversing the other.
//! The trend is the side of the more recent cross, missing until both crosses have come. A long is closed, by a market
//! order, once the price has been 2% over the last cross's close and the low then crosses under a trailing stop: the highest
//! of the bar's low less 3%, never under what it was, kept while the trend lasts (the mirror for a short, 2% under and the
//! high over a stop from the high plus 3%). Each new trend clears the 2% memory. 0.075% commission; starts with 100,000.
//!
//! The rule looks ahead: it asks to be run again after each of its fills inside a bar (`calc_on_order_fills`), reading that
//! bar's final values, so a signal the bar's close makes can be traded inside that bar, a value the bar could not yet have
//! had. Written so with `after_fill`. Pyramiding is 1.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Sma;
use vike_indicators::strategy_kit::{max, min};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::helpers::{BarsSince, fewer};
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::floor_printed;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const SIZE: f64 = 100.0;
const TRAIL: f64 = 3.0 * 0.01;
const TRIGGER: f64 = 2.0 * 0.01;

struct S {
    fast: Sma,
    slow: Sma,
    cross: Cross,
    since_buy: BarsSince,
    since_sell: BarsSince,
    buy_trend: bool,
    sell_trend: bool,
    entry_price: f64,
    long_hit: bool,
    short_hit: bool,
    long_stop: f64,
    short_stop: f64,
    low_under: Cross,
    high_over: Cross,
    /// The bar the indicators have seen: a fill inside a bar feeds them ahead of its close.
    fed: Option<i64>,
    enter_long: bool,
    enter_short: bool,
    close_long: bool,
    close_short: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: Sma::new(15.0).expect("a valid length"),
        slow: Sma::new(45.0).expect("a valid length"),
        cross: Cross::new(),
        since_buy: BarsSince::default(),
        since_sell: BarsSince::default(),
        buy_trend: false,
        sell_trend: false,
        entry_price: f64::NAN,
        long_hit: false,
        short_hit: false,
        long_stop: f64::NAN,
        short_stop: f64::NAN,
        low_under: Cross::new(),
        high_over: Cross::new(),
        fed: None,
        enter_long: false,
        enter_short: false,
        close_long: false,
        close_short: false,
    })
}

impl S {
    fn feed(&mut self, bar: &Bar) {
        let (fast, slow) = (self.fast.next(bar), self.slow.next(bar));
        let signal = self.cross.step(fast, slow);
        let enter_long = matches!(signal, Signal::Long);
        let enter_short = matches!(signal, Signal::Short);
        let since_buy = self.since_buy.step(enter_long);
        let since_sell = self.since_sell.step(enter_short);
        let buy_trend = fewer(since_buy, since_sell);
        let sell_trend = fewer(since_sell, since_buy);
        let change_trend = (buy_trend && self.sell_trend) || (sell_trend && self.buy_trend);
        if enter_long || enter_short {
            self.entry_price = bar.close;
        }
        let trigger = if buy_trend {
            self.entry_price * (1.0 + TRIGGER)
        } else {
            self.entry_price * (1.0 - TRIGGER)
        };
        if buy_trend && bar.high >= trigger {
            self.long_hit = true;
        }
        if sell_trend && bar.low <= trigger {
            self.short_hit = true;
        }
        // The crosses against the stops of the bar before: `crossunder(low, longStopPrice[1])`.
        let under = matches!(self.low_under.step(bar.low, self.long_stop), Signal::Short);
        let over = matches!(self.high_over.step(bar.high, self.short_stop), Signal::Long);
        self.close_long = buy_trend && under && self.long_hit;
        self.close_short = sell_trend && over && self.short_hit;
        self.long_stop = if buy_trend { max(bar.low * (1.0 - TRAIL), self.long_stop) } else { 0.0 };
        self.short_stop =
            if sell_trend { min(bar.high * (1.0 + TRAIL), self.short_stop) } else { 999_999.0 };
        if change_trend {
            self.long_hit = false;
            self.short_hit = false;
        }
        (self.enter_long, self.enter_short) = (enter_long, enter_short);
        (self.buy_trend, self.sell_trend) = (buy_trend, sell_trend);
    }

    fn rule(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        if self.fed != Some(bar.timestamp) {
            self.fed = Some(bar.timestamp);
            self.feed(bar);
        }
        if self.enter_long {
            strategy.entry("BEST L", Long).size(floor_printed(SIZE, sizing.qty_step()));
        }
        if self.close_long {
            strategy.close("BEST L");
        }
        if self.enter_short {
            strategy.entry("BEST S", Short).size(floor_printed(SIZE, sizing.qty_step()));
        }
        if self.close_short {
            strategy.close("BEST S");
        }
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.075), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        self.rule(strategy, bar);
    }

    fn after_fill(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        self.rule(strategy, bar);
    }
}
