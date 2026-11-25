// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "TrendMaster Pro 2.3 with Alerts" by everget, https://www.tradingview.com/script/mVkDf8qh/
// The original's notice: (c) @alexgrover
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2025-06-02, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Long 5 when SMA(9) of the close crosses up through SMA(21) and every filter agrees: the Bollinger bands (SMA(20), 2
//! population deviations) wider than twice ATR(20), the close over the middle band, RSI(14) over 55, the MACD(12, 26, 9) line
//! over its signal, the 3-bar SMA of the 14-bar stochastic (%K) over 20 and over its own 3-bar SMA, and ADX(14, 14) over 25.
//! Short 5 on the cross down with the mirror filters (under the middle band, RSI under 45, MACD under its signal, %K under
//! 80 and under its SMA, the same width and ADX). An entry is taken only while flat or held the other way (which it first
//! closes at this bar's close), and only inside a session (UTC, Monday to Friday: 18:00-24:00, 00:00-08:00, 08:00-13:00,
//! 13:00-18:00) while fewer than 5 trades have closed since that session began.
//!
//! The stop is 1% past the signal bar's close. On the bar the position first shows, an exit rests from the next bar: that stop
//! and a target twice the stop's distance from the average entry price on the other side. A long also closes at the bar's
//! close on a short signal, a short on a long one. On the bar opening at 18:00 every order is cancelled and the position
//! closed at that bar's close. No costs; starts with 1,000,000 and a margin of 100% of a position on either side.
//!
//! Read while porting: the original forgets the stop of a side on the bar that side's position has gone, at the end of the bar,
//! so an entry signalled on that bar gets no exit; it is ported so. Its counters of stops and targets hit never count (they
//! test the stop after forgetting it), so they never stop an entry and are left out. The session 18:00-24:00 ends at the
//! midnight of its own weekday, so it runs Monday to Friday evening. Left out: the support and resistance
//! levels (a weekly series read with lookahead, drawn only, never deciding an order), the bands of the highs and lows and the
//! moving-average cross value (read only by stop options the defaults do not take), the automatic risk size (off), plots,
//! alerts and labels.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Adx, Atr, Bollinger, Macd, Rsi, Sma, Stochastic};
use vike_marketdata::Bar;
use vike_model::MS_PER_DAY;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::helpers::weekday;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const LONG: &str = "Long Entry";
const SHORT: &str = "Short Entry";
const SIZE: f64 = 5.0;
const STOP_PERCENT: f64 = 1.0;
const REWARD: f64 = 2.0;
const TRADES_PER_SESSION: usize = 5;
/// Each session's first and last minute (exclusive) of the UTC day.
const SESSIONS: [(i64, i64); 4] =
    [(18 * 60, 24 * 60), (0, 8 * 60), (8 * 60, 13 * 60), (13 * 60, 18 * 60)];
const MARKET_CLOSE: i64 = 18 * 60;

struct S {
    fast: Sma,
    slow: Sma,
    cross: Cross,
    bands: Bollinger,
    width_atr: Atr,
    rsi: Rsi,
    macd: Macd,
    stochastic: Stochastic,
    adx: Adx,
    /// Whether the bar before was in each session, and the closed trades when each began.
    in_before: [bool; 4],
    began_at: [usize; 4],
    held_before: f64,
    long_stop: f64,
    short_stop: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: Sma::new(9.0).expect("a valid length"),
        slow: Sma::new(21.0).expect("a valid length"),
        cross: Cross::new(),
        bands: Bollinger::new(20.0, 2.0).expect("valid parameters"),
        width_atr: Atr::new(20.0).expect("a valid length"),
        rsi: Rsi::new(14.0).expect("a valid length").with_rs_form(true),
        macd: Macd::new(12.0, 26.0, 9.0).expect("valid lengths"),
        stochastic: Stochastic::new(14.0, 3.0, 3.0).expect("valid lengths"),
        adx: Adx::new(14.0).expect("a valid length"),
        in_before: [false; 4],
        began_at: [0; 4],
        held_before: 0.0,
        long_stop: f64::NAN,
        short_stop: f64::NAN,
    })
}

/// Which of the four sessions the bar opening at `ts` is in, each on Monday to Friday.
fn sessions(ts: i64) -> [bool; 4] {
    let minute = ts.rem_euclid(MS_PER_DAY) / 60_000;
    SESSIONS.map(|(from, to)| weekday(ts) < 5 && minute >= from && minute < to)
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let cross = self.cross.step(self.fast.next(bar), self.slow.next(bar));
        let [upper, basis, lower] = self.bands.next(bar);
        let width = self.width_atr.next(bar);
        let rsi = self.rsi.next(bar);
        let [macd, signal, _] = self.macd.next(bar);
        let [k, d] = self.stochastic.next(bar);
        let [adx, _, _] = self.adx.next(bar);
        let common = upper - lower > width * 2.0 && adx > 25.0;
        let buy = matches!(cross, Signal::Long)
            && common
            && bar.close > basis
            && rsi > 55.0
            && macd > signal
            && k > 20.0
            && k > d;
        let sell = matches!(cross, Signal::Short)
            && common
            && bar.close < basis
            && rsi < 45.0
            && macd < signal
            && k < 80.0
            && k < d;

        let closed = strategy.closed_trades().map_or(0, <[_]>::len);
        let now = sessions(bar.timestamp);
        for ((began, &is_in), &was_in) in self.began_at.iter_mut().zip(&now).zip(&self.in_before) {
            if is_in && !was_in {
                *began = closed;
            }
        }
        self.in_before = now;
        let can_enter = now
            .iter()
            .zip(&self.began_at)
            .any(|(&is_in, &began)| is_in && closed < began + TRADES_PER_SESSION);

        let held = strategy.position();
        let mut closing = false;
        if bar.timestamp.rem_euclid(MS_PER_DAY) / 60_000 == MARKET_CLOSE {
            strategy.cancel_all();
            strategy.close_all().here();
            closing = true;
        }
        if held <= 0.0 && buy && can_enter {
            if held < 0.0 && !closing {
                strategy.close(SHORT).here();
                closing = true;
            }
            self.long_stop = bar.close * (1.0 + -STOP_PERCENT / 100.0);
            strategy.entry(LONG, Long).size(SIZE);
        }
        if self.held_before <= 0.0 && held > 0.0 && !self.long_stop.is_nan() {
            let average = strategy.avg_cost();
            let take = average + REWARD * (average - self.long_stop).abs();
            strategy.exit("Long Exit").from(LONG).take(take).stop(self.long_stop);
        }
        if held >= 0.0 && sell && can_enter {
            if held > 0.0 && !closing {
                strategy.close(LONG).here();
                closing = true;
            }
            self.short_stop = bar.close * (1.0 + STOP_PERCENT / 100.0);
            strategy.entry(SHORT, Short).size(SIZE);
        }
        if self.held_before >= 0.0 && held < 0.0 && !self.short_stop.is_nan() {
            let average = strategy.avg_cost();
            let take = average - REWARD * (average - self.short_stop).abs();
            strategy.exit("Short Exit").from(SHORT).take(take).stop(self.short_stop);
        }
        if held > 0.0 && sell && !closing {
            strategy.close(LONG).here();
        }
        if held < 0.0 && buy && !closing {
            strategy.close(SHORT).here();
        }
        if self.held_before > 0.0 && held <= 0.0 {
            self.long_stop = f64::NAN;
        }
        if self.held_before < 0.0 && held >= 0.0 {
            self.short_stop = f64::NAN;
        }
        self.held_before = held;
    }
}
