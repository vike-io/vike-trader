// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Strategy Follow Line MTF Any MM Average" by Dreadblitz, https://www.tradingview.com/script/6lhyIPGA/
// The original's notice: © Dreadblitz
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2020-04-17, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! A trend line from Bollinger-style bands: EMA(21) of the close plus and minus one population standard deviation of 21
//! closes. A close above the upper band puts the line at the low less ATR(5), never lower than it was; a close under the
//! lower band at the high plus ATR(5), never higher; otherwise it stays. The trend turns up when the line rises and down
//! when it falls. Long 1 when it turns up, short 1 when it turns down, each reversing the other. No costs; starts with
//! 10,000 in a USD account.
//!
//! The rule looks ahead: it asks to be run again after each of its fills inside a bar (`calc_on_order_fills`), reading that
//! bar's final values, so a turn the bar's close makes can be traded inside that bar, a value the bar could not yet have
//! had. Written so with `after_fill`. Its timeframe call is on the chart's own timeframe, so it is the chart's values.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Atr, Ema, Stddev};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const LENGTH: f64 = 21.0;

struct S {
    average: Ema,
    deviation: Stddev,
    atr: Atr,
    line: f64,
    trend: f64,
    /// The bar the indicators have seen: a fill inside a bar feeds them ahead of its close.
    fed: Option<i64>,
    buy: bool,
    sell: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        average: Ema::new(LENGTH).expect("a valid length"),
        deviation: Stddev::new(LENGTH).expect("a valid length"),
        atr: Atr::new(5.0).expect("a valid length"),
        line: f64::NAN,
        trend: f64::NAN,
        fed: None,
        buy: false,
        sell: false,
    })
}

impl S {
    fn feed(&mut self, bar: &Bar) {
        let average = self.average.next(bar);
        let deviation = self.deviation.next(bar);
        let atr = self.atr.next(bar);
        let (upper, lower) = (average + deviation, average - deviation);
        let last = self.line;
        let mut line = last;
        if bar.close > upper {
            line = bar.low - atr;
            if line < last {
                line = last;
            }
        } else if bar.close < lower {
            line = bar.high + atr;
            if line > last {
                line = last;
            }
        }
        let mut trend = self.trend;
        if line > last {
            trend = 1.0;
        }
        if line < last {
            trend = -1.0;
        }
        self.buy = self.trend == -1.0 && trend == 1.0;
        self.sell = self.trend == 1.0 && trend == -1.0;
        (self.line, self.trend) = (line, trend);
    }

    fn rule(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        if self.fed != Some(bar.timestamp) {
            self.fed = Some(bar.timestamp);
            self.feed(bar);
        }
        if self.buy {
            strategy.entry("long", Long).size(1.0);
        }
        if self.sell {
            strategy.entry("short", Short).size(1.0);
        }
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        self.rule(strategy, bar);
    }

    fn after_fill(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        self.rule(strategy, bar);
    }
}
