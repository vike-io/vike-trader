// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Tripple super Trend Strategy" by TradingStrategyCheck, https://www.tradingview.com/script/aCbQTE0W/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2021-05-31, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! An EMA(200) of the close on 15-minute bars as the trend, and a stochastic RSI: `k` the 3-bar SMA of the RSI(14) placed in
//! its 14-bar range, `d` the 3-bar SMA of `k`. Flat, a long of the account's whole worth when the close is above the trend,
//! `k` crosses up through `d` and `k` is under 28; a short when the close is under the trend, `k` crosses down through `d`
//! and `k` is over 78. Held, a long rests a stop at the lowest low of the 30 bars up to the bar its entry filled on, and a
//! target twice that distance above the average cost; a short a stop at the highest high of the 25 bars up to its fill's
//! bar and a target 1.5 times that distance below. No costs; starts with 5,000 dollars, the account kept in USD. The rule's
//! date range (2020 to 9999) holds on every bar here. **Does not reproduce its reference** (143 trades, net 2772.349609 on
//! BTCUSDT 30m, 2025-01-01..2026-08-25): this file gives 145 / 2743.633195, two trades more and the net 28.72 under, no
//! cause found (the 15-minute reading at the first or the last lower bar, the RSI's form and the stochastic's construct
//! move the net, never the count); the owner accepted those numbers on 2026-11-16.
//!
//! The original's three supertrends (factors 1, 2 and 3 of ATR 10, 11 and 12) reach its conditions only as truth values,
//! `close > trend1 and trend2 or ...`, and a direction of 1 or -1 is always true: its long conditions hold whatever they
//! read, its second short condition never (`close < trend`, a close under 1), so no order reads a supertrend and they are
//! left out, as is that never-true short. Its two long conditions are the same one; it places its entry once.
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

use vike_indicators::indicators::{Donchian, Ema, Rsi, Stochastic};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::higher::HigherTimeframeSourceUnseen;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const FIFTEEN_MINUTES: &str = "15m";
const TREND: f64 = 200.0;
const OVERSOLD: f64 = 28.0;
const OVERBOUGHT: f64 = 78.0;
const LONG_LOOKBACK: f64 = 30.0;
const SHORT_LOOKBACK: f64 = 25.0;
const LONG_REWARD: f64 = 2.0;
const SHORT_REWARD: f64 = 1.5;

struct S {
    /// The rule's first bar's open: no 15-minute bar before it is fed.
    from: Option<i64>,
    unseen: HigherTimeframeSourceUnseen,
    trend: Ema,
    trend_now: f64,
    rsi: Rsi,
    stoch: Stochastic,
    cross: Cross,
    lows: Donchian,
    highs: Donchian,
    /// The lookbacks' extremes as they stood on the bar the position last grew, and last shrank.
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
        rsi: Rsi::new(14.0).expect("a valid length"),
        stoch: Stochastic::new(14.0, 3.0, 3.0).expect("valid lengths"),
        cross: Cross::new(),
        lows: Donchian::new(LONG_LOOKBACK).expect("a valid length"),
        highs: Donchian::new(SHORT_LOOKBACK).expect("a valid length"),
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
        let trend = self.trend_now;
        let close = bar.close;
        let rsi = self.rsi.next(bar);
        let [k, d] = self.stoch.next_hlc(rsi, rsi, rsi);
        let cross = self.cross.step(k, d);
        let [_, _, low] = self.lows.next(bar);
        let [high, _, _] = self.highs.next(bar);

        // Each stop level is kept as it stood on the bar the position grew (a long's fill) or shrank (a short's fill).
        let held = strategy.position();
        if held > self.held_before {
            self.long_stop = low;
        }
        if held < self.held_before {
            self.short_stop = high;
        }
        self.held_before = held;

        if held == 0.0 {
            let size = share_of_worth(&strategy.sizing(), 100.0, close);
            if close > trend && matches!(cross, Signal::Long) && k < OVERSOLD {
                strategy.entry("long1", Long).size(size);
            }
            if close < trend && matches!(cross, Signal::Short) && k > OVERBOUGHT {
                strategy.entry("short1", Short).size(size);
            }
        }
        let cost = strategy.avg_cost();
        if held > 0.0 {
            let stop = self.long_stop;
            strategy
                .exit("TP/ SL")
                .from("long1")
                .stop(stop)
                .take(cost + (cost - stop) * LONG_REWARD);
        }
        if held < 0.0 {
            let stop = self.short_stop;
            strategy
                .exit("TP/ SL")
                .from("short1")
                .stop(stop)
                .take(cost - (stop - cost) * SHORT_REWARD);
        }
    }
}
