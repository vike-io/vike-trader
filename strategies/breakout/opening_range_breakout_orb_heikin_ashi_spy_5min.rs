// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Opening Range Breakout (ORB) Heikin Ashi SPY 5min Correlation Strategy" by exlux, https://www.tradingview.com/script/QpJvYDzb/
// The original's notice: © exlux99
// Ported into this file: the library "ta" (TradingView/ta/7), MPL-2.0; its notice: © TradingView
// Ported into this file: the library "RelativeValue" (TradingView/RelativeValue/2), MPL-2.0; its notice: © TradingView
// Changed: rewritten by hand in Rust for vike.

//! An opening-range breakout of the chart's market read against SPY's range. The opening session is 09:30 to 10:00, the
//! trading session 10:00 to 12:00 and the closing one 15:50 to 15:55, New York time (the US daylight rule; a bar belongs to a
//! session by its open, Monday to Sunday). SPY's high and low on each chart bar are those of its bars inside the chart bar
//! (the input `CORRELATION`, its one-minute bars), kept from the last chart bar that had any. The range is SPY's high and low
//! on the opening session's first bar, read from the bar after it (on an opening-session bar widened by that bar's own,
//! for that bar only). In the trading session a close over the range's high sets the long
//! mark, a close under its low the short mark, each only while the other is unset and the relative volume is over 1: the
//! volume since the week began (a Monday, New York time) over the mean of the same reading of the last three weeks at the
//! same time into the week (the library's `relativeVolume(3, "W", true)`). On the closing session both marks clear and
//! everything is closed at the next open. While a mark is set, its entry is issued on every bar: one unit long or short,
//! one entry a side. No costs; starts with 1,000,000, language version 5. The Heikin-Ashi reading of SPY (off by default),
//! the plots and the labels are left out.
//!
//! **Data:** the original names SPY; this file reads Dukascopy's `SPY.CFD` (the venue's `SPY.US/USD`), a CFD, not the
//! exchange stock (a profile's `[inputs]` table maps `CORRELATION` to another series). The reference ran on Binance's
//! BTCUSDT 30-minute bars, whose exchange time is UTC, and made no trade: on 30-minute bars no bar opens inside the
//! five-minute closing session, and a BTCUSDT close is never under SPY's low, so the one long it opens is never closed
//! (an open trade is in neither number).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;

use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::helpers::us_minute;
use vike_tactics::{Rule, RuleDefaults, Tactics};

/// The series whose range the breakout is read against: SPY's one-minute bars.
const CORRELATION: &str = "dukascopy:SPY.CFD:1m";
/// The sessions, in minutes of the New York day: each from its first minute up to, not including, its last.
const OPENING: (i64, i64) = (9 * 60 + 30, 10 * 60);
const TRADING: (i64, i64) = (10 * 60, 12 * 60);
const CLOSING: (i64, i64) = (15 * 60 + 50, 15 * 60 + 55);
/// The weeks the relative volume averages.
const WEEKS: usize = 3;
const DAY_MS: i64 = 86_400_000;

/// The values one week collected: each bar's open time and its cumulative volume since the week began.
#[derive(Default)]
struct Week {
    start: i64,
    times: Vec<i64>,
    data: Vec<f64>,
}

impl Week {
    /// The value at `offset` into the week, as the library's `calcAverageByTime` reads it: at the first bar opening exactly
    /// then, else at the last bar before it (`array.binary_search_leftmost`), and at the week's last bar when none opened by
    /// then (an index of -1 read from the end).
    fn at(&self, offset: i64) -> f64 {
        let target = self.start + offset;
        let before = self.times.partition_point(|&t| t < target);
        let index = if self.times.get(before) == Some(&target) {
            Some(before)
        } else {
            before.checked_sub(1)
        };
        index.map_or_else(|| self.data.last().copied().unwrap_or(f64::NAN), |i| self.data[i])
    }
}

/// The library's `relativeVolume(length, "W", true)`: the volume since the week's first bar over the mean of the last
/// `WEEKS` weeks' at the same offset into the week; NaN before a week has been collected.
#[derive(Default)]
struct RelativeVolume {
    week: Option<i64>,
    sum: f64,
    past: VecDeque<Week>,
    current: Week,
}

impl RelativeVolume {
    fn next(&mut self, bar: &Bar) -> f64 {
        let start = week_start(bar.timestamp);
        let anchor = self.week.replace(start).is_some_and(|before| before != start);
        if self.current.times.is_empty() && !anchor {
            self.current.start = start;
        }
        if anchor {
            self.sum = 0.0;
            self.past.push_back(std::mem::take(&mut self.current));
            if self.past.len() > WEEKS {
                self.past.pop_front();
            }
            self.current.start = start;
        }
        self.sum += bar.volume;
        self.current.times.push(bar.timestamp);
        self.current.data.push(self.sum);
        let offset = bar.timestamp - self.current.start;
        let total: f64 = self.past.iter().map(|week| week.at(offset)).sum();
        self.sum / (total / self.past.len() as f64)
    }
}

/// The New York midnight of the Monday that begins the week of `ts`, in epoch ms.
fn week_start(ts: i64) -> i64 {
    let midnight = ts - us_minute(ts, -5) * 60_000;
    // 1970-01-01, day 0, was a Thursday: Monday is three days on.
    let day = midnight.div_euclid(DAY_MS);
    let monday = day - (day + 3).rem_euclid(7);
    midnight - (day - monday) * DAY_MS
}

struct S {
    volume: RelativeVolume,
    /// Whether the bar before was in the opening session, `None` on the first bar.
    was_opening: Option<bool>,
    /// SPY's high and low on the last chart bar that had a SPY bar inside it.
    correlation: (f64, f64),
    /// The range as of the bar before (`prevSessionHigh`, `prevSessionLow`).
    range: (f64, f64),
    long: bool,
    short: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        volume: RelativeVolume::default(),
        was_opening: None,
        correlation: (f64::NAN, f64::NAN),
        range: (f64::NAN, f64::NAN),
        long: false,
        short: false,
    })
}

fn in_session(minute: i64, (from, to): (i64, i64)) -> bool {
    (from..to).contains(&minute)
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn inputs(&self) -> Vec<String> {
        vec![CORRELATION.to_string()]
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let inside = strategy.input(CORRELATION).since(bar.timestamp);
        if !inside.is_empty() {
            let high = inside.iter().map(|b| b.high).fold(f64::NEG_INFINITY, f64::max);
            let low = inside.iter().map(|b| b.low).fold(f64::INFINITY, f64::min);
            self.correlation = (high, low);
        }
        let (high, low) = self.correlation;
        let minute = us_minute(bar.timestamp, -5);
        let opening = in_session(minute, OPENING);
        // `inSession and not inSession[1]`; on the first bar the bar before reads as out of it.
        let was_opening = self.was_opening.replace(opening);
        let opens = opening && was_opening != Some(true);
        // The range read is the one the bar before carried (`[1]`), widened by this bar inside the opening session.
        let (mut range_high, mut range_low) = self.range;
        if opens {
            self.range = (high, low);
        }
        if opening && high > range_high {
            range_high = high;
        }
        if opening && low < range_low {
            range_low = low;
        }
        let relative_volume = self.volume.next(bar);
        let trading = in_session(minute, TRADING);
        if bar.close > range_high && trading && !self.short && relative_volume > 1.0 {
            self.long = true;
        }
        if bar.close < range_low && trading && !self.long && relative_volume > 1.0 {
            self.short = true;
        }
        let closing = in_session(minute, CLOSING);
        if closing {
            self.long = false;
            self.short = false;
        }
        if self.long {
            strategy.entry("long", Long);
        }
        if self.short {
            strategy.entry("short", Short);
        }
        if closing {
            strategy.close_all();
        }
    }
}
