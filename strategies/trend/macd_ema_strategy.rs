// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "MACD EMA Strategy" by TradingStrategyCheck, https://www.tradingview.com/script/8WbIRZkS/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2021-06-06, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! An EMA(206) of the close on 15-minute bars as the trend, and a MACD of simple averages (SMA(15) less SMA(24) of the close,
//! its signal line the SMA(9) of that). Flat, a long of the account's whole worth when this close and the one before are both
//! above the trend as each bar read it, the MACD is under zero and crosses up through its signal; a short, the mirror. Held,
//! a long rests a stop at the lowest low of the 46 bars before the bar its entry filled on, and a target 1.9 times that
//! distance above the average cost; a short a stop at the highest high of the 25 bars before its fill's bar and a target 2.1
//! times that distance below. No costs; starts with 5,000 dollars, the account kept in USD. The rule's date range (2020 to
//! 9999) holds on every bar here.
//!
//! The 15-minute frame is an input of the run's own symbol at that bar length (decision 0271), read at the chart bar's
//! close: the EMA over its bars from the rule's first bar on, as it stands after the last of them inside the chart bar.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Donchian, Ema, Sma};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::higher::HigherTimeframeSourceUnseen;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const FIFTEEN_MINUTES: &str = "15m";
const TREND: f64 = 206.0;
const FAST: f64 = 15.0;
const SLOW: f64 = 24.0;
const SIGNAL: f64 = 9.0;
const LONG_LOOKBACK: f64 = 46.0;
const SHORT_LOOKBACK: f64 = 25.0;
const LONG_REWARD: f64 = 1.9;
const SHORT_REWARD: f64 = 2.1;

struct S {
    /// The rule's first bar's open: no 15-minute bar before it is fed.
    from: Option<i64>,
    unseen: HigherTimeframeSourceUnseen,
    trend: Ema,
    /// The trend as this bar and the bar before read it, and the bar before's close.
    trend_now: f64,
    trend_before: f64,
    close_before: f64,
    fast: Sma,
    slow: Sma,
    signal: Sma,
    cross: Cross,
    lows: Donchian,
    highs: Donchian,
    /// The lowest low and highest high of the lookbacks up to the bar before this one.
    low_before: f64,
    high_before: f64,
    /// Those levels as they stood on the bar the position last grew, and last shrank.
    long_stop: f64,
    short_stop: f64,
    held_before: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        from: None,
        unseen: HigherTimeframeSourceUnseen::new(),
        trend: Ema::new(TREND).expect("a valid length"),
        trend_now: f64::NAN,
        trend_before: f64::NAN,
        close_before: f64::NAN,
        fast: Sma::new(FAST).expect("a valid length"),
        slow: Sma::new(SLOW).expect("a valid length"),
        signal: Sma::new(SIGNAL).expect("a valid length"),
        cross: Cross::new(),
        lows: Donchian::new(LONG_LOOKBACK).expect("a valid length"),
        highs: Donchian::new(SHORT_LOOKBACK).expect("a valid length"),
        low_before: f64::NAN,
        high_before: f64::NAN,
        long_stop: f64::NAN,
        short_stop: f64::NAN,
        held_before: 0.0,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn inputs(&self) -> Vec<String> {
        vec![FIFTEEN_MINUTES.to_string()]
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let from = *self.from.get_or_insert(bar.timestamp);
        let quarters = self.unseen.next(strategy.input(FIFTEEN_MINUTES));
        for quarter in &quarters[quarters.partition_point(|b| b.timestamp < from)..] {
            self.trend_now = self.trend.next(quarter);
        }
        let (trend, trend_before) = (self.trend_now, self.trend_before);
        let close = bar.close;
        let macd = self.fast.next(bar) - self.slow.next(bar);
        let signal = self.signal.next_value(macd);
        let cross = self.cross.step(macd, signal);

        // The stop levels are each lookback's extreme up to the bar before; each is kept as it stood on the bar the
        // position grew (a long's fill) or shrank (a short's fill).
        let held = strategy.position();
        if held > self.held_before {
            self.long_stop = self.low_before;
        }
        if held < self.held_before {
            self.short_stop = self.high_before;
        }
        self.held_before = held;
        let [_, _, low] = self.lows.next(bar);
        let [high, _, _] = self.highs.next(bar);
        (self.low_before, self.high_before) = (low, high);

        if held == 0.0 {
            let size = share_of_worth(&strategy.sizing(), 100.0, close);
            if close > trend
                && self.close_before > trend_before
                && macd < 0.0
                && matches!(cross, Signal::Long)
            {
                strategy.entry("long", Long).size(size);
            }
            if close < trend
                && self.close_before < trend_before
                && macd > 0.0
                && matches!(cross, Signal::Short)
            {
                strategy.entry("short", Short).size(size);
            }
        }
        let cost = strategy.avg_cost();
        if held > 0.0 {
            let stop = self.long_stop;
            strategy
                .exit("TP/ SL")
                .from("long")
                .stop(stop)
                .take(cost + (cost - stop) * LONG_REWARD);
        }
        if held < 0.0 {
            let stop = self.short_stop;
            strategy
                .exit("TP/ SL")
                .from("short")
                .stop(stop)
                .take(cost - (stop - cost) * SHORT_REWARD);
        }
        (self.trend_before, self.close_before) = (trend, close);
    }
}
