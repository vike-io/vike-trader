// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Buy&Sell Bullish Engulfing Strategy [The Quant Science]" by thequantscience, https://www.tradingview.com/script/0moOi6G5/
// The original's notice: © thequantscience
// Changed: rewritten by hand in Rust for vike.

//! A bullish engulfing candle under SMA(50): a white body longer than its 14-bar EMA after a black one shorter than it, the
//! close at or over the last open and the open at or under the last close, one of them strictly. While no trade is open, it
//! rests a limit buy at its close for 30% of the starting 10,000 worth. When the account's worth crosses up through 102% of what
//! it was at that signal, a limit sell rests at this close for the whole lot; when it crosses down through 98%, another. Every
//! order is judged at the close it was placed on. 0.07% commission; starts with 10,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Ema;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const NAME: &str = "#ENTRY";
const CAPITAL: f64 = 10_000.0;
const ORDER_SHARE: f64 = 30.0;
const PROFIT: f64 = 2.0;
const STOP: f64 = 2.0;

struct S {
    trend: IndicatorLine,
    body_average: Ema,
    /// The last bar's open, close, and whether its body was short.
    last: (f64, f64, bool),
    exit_at: f64,
    stop_at: f64,
    up: Cross,
    down: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        trend: IndicatorLine::new("sma", &[50.0]),
        body_average: Ema::new(14.0).expect("a valid length"),
        last: (f64::NAN, f64::NAN, false),
        exit_at: 0.0,
        stop_at: 0.0,
        up: Cross::new(),
        down: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.07), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let (o, c) = (bar.open, bar.close);
        let average = self.trend.next(bar)[0];
        let body = c.max(o) - c.min(o);
        let body_average = self.body_average.next_value(body);
        let small = body < body_average;
        let (lo, lc, last_small) = std::mem::replace(&mut self.last, (o, c, small));
        let engulfing = c < average
            && o < c
            && body > body_average
            && lo > lc
            && last_small
            && c >= lo
            && o <= lc
            && (c > lo || o < lc);
        if engulfing && strategy.lots().is_empty() {
            let worth = strategy.equity();
            self.exit_at = worth + (worth * PROFIT / 100.0);
            self.stop_at = worth - (worth * STOP / 100.0);
            let qty = sizing.floor(((CAPITAL * ORDER_SHARE) / 100.0) / c);
            strategy.entry(NAME, Long).size(qty).limit(c);
        }
        let worth = strategy.equity();
        if matches!(self.up.step(worth, self.exit_at), Signal::Long) {
            strategy.exit("#TP").from(NAME).take(c);
        }
        if matches!(self.down.step(worth, self.stop_at), Signal::Short) {
            strategy.exit("#SL").from(NAME).take(c);
        }
    }
}
