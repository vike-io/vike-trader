// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Chandelier Exit - Strategy" by melihtuna, https://www.tradingview.com/script/yca6gmSk/
// The original's notice: © melihtuna
// Changed: rewritten by hand in Rust for vike.

//! A chandelier exit on the close (22 bars, 3 ATR): its direction turns up when the close goes over the last short stop and
//! down when it goes under the last long stop, each stop carried from the last bar's while the last close stayed beyond it. A
//! turn up goes long 10,000 dollars' worth (and closes the short), a turn down short (and closes the long), only on the bars
//! from 2019-01-01 to 2025-01-01. A position also closes on a loss of 5% at the close, a gain of 20% at the high (a short: the
//! low), or a trailing stop: once the best gain has passed 10%, a fall of 5 points from it. Each at the next open. 0.03%
//! commission; starts with 10,000 in a USD account.
//!
//! Read while porting: the account is kept in USD on a USDT-quoted symbol, and the trades from 2019 come before the first
//! USDT-USD rate (2021-05-04): before it one USDT is one dollar, the owner's ruling of 2026-11-16 (decision 0243 amended), and
//! with it the row matches (59 trades, 11224.350159 against 11224.357422); booked in USDT throughout it is 11239.92. The
//! labels and the highlighting are left out.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Atr, Donchian};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::calendar::timestamp;
use vike_tactics::sizing::cash_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const LENGTH: f64 = 22.0;
const MULT: f64 = 3.0;
const CASH: f64 = 10_000.0;

struct S {
    atr: Atr,
    closes: Donchian,
    long_stop: f64,
    short_stop: f64,
    /// The last bar's close.
    close: f64,
    direction: i8,
    last_direction: Option<i8>,
    /// The best gain of the long and the best (lowest) result of the short, in percent, as the last bar left them.
    best_long: f64,
    best_short: f64,
    from: i64,
    to: i64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        atr: Atr::new(LENGTH).expect("a valid length"),
        closes: Donchian::new(LENGTH).expect("a valid length"),
        long_stop: f64::NAN,
        short_stop: f64::NAN,
        close: f64::NAN,
        direction: 1,
        last_direction: None,
        best_long: 0.0,
        best_short: 0.0,
        from: timestamp(2019, 1, 1, 0, 0),
        to: timestamp(2025, 1, 1, 0, 0),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.03), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let atr = MULT * self.atr.next(bar);
        let [highest, _, lowest] = self.closes.next_hl(bar.close, bar.close);
        let long_stop = highest - atr;
        let long_last = if self.long_stop.is_nan() { long_stop } else { self.long_stop };
        self.long_stop = if self.close > long_last { long_stop.max(long_last) } else { long_stop };
        let short_stop = lowest + atr;
        let short_last = if self.short_stop.is_nan() { short_stop } else { self.short_stop };
        self.short_stop =
            if self.close < short_last { short_stop.min(short_last) } else { short_stop };
        self.close = bar.close;
        if bar.close > short_last {
            self.direction = 1;
        } else if bar.close < long_last {
            self.direction = -1;
        }
        let buy = self.direction == 1 && self.last_direction == Some(-1);
        let sell = self.direction == -1 && self.last_direction == Some(1);
        self.last_direction = Some(self.direction);

        let (held, average) = (strategy.position(), strategy.avg_cost());
        let high_gain = (bar.high - average) / average * 100.0;
        let close_gain = (bar.close - average) / average * 100.0;
        let low_gain = (bar.low - average) / average * 100.0;
        let (best_long, best_short) = (self.best_long, self.best_short);
        self.best_long = match held > 0.0 {
            true if high_gain > best_long => high_gain,
            true => best_long,
            false => 0.0,
        };
        self.best_short = match held < 0.0 {
            true if close_gain < best_short => close_gain,
            true => best_short,
            false => 0.0,
        };

        let on = bar.timestamp >= self.from && bar.timestamp <= self.to;
        let size = cash_worth(&strategy.sizing(), CASH, bar.close);
        if on && buy {
            strategy.entry("LONG", Long).size(size);
        }
        if on && sell {
            strategy.close("LONG");
        }
        if close_gain < -5.0
            || high_gain > 20.0
            || (best_long > 10.0 && best_long - high_gain > 5.0)
        {
            strategy.close("LONG");
        }
        if on && sell {
            strategy.entry("SHORT", Short).size(size);
        }
        if on && buy {
            strategy.close("SHORT");
        }
        if close_gain > 5.0
            || low_gain < -20.0
            || (-best_short > 10.0 && best_short - close_gain < -5.0)
        {
            strategy.close("SHORT");
        }
    }
}
