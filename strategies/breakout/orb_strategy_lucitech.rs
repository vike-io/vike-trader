// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "ORB Strategy [LuciTech]" by TradesLuci, https://www.tradingview.com/script/10yYqaY7/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2026-02-28, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! An opening range breakout. The range is the bar that opens at 9:30 New York time (13:30 UTC in summer time, 14:30 in
//! winter), a 30-minute bar standing for the rule's 15 minutes; its high, low and middle stay until the next day's range.
//! From the range's close until 11:00 New York time, and again from midnight UTC until the next range (the cutoff is
//! compared with the bar's own date, so it is not yet past), a close above the range's high with the close before at or
//! under it is a long breakout, a close under its low with the close before at or over it a short one, each once a day
//! (the flag resets with the next range, and is spent by a breakout whether or not a trade is taken).
//!
//! With no position, a breakout enters risking 2% of the account's worth: the stop is the signal bar's low (its high for a
//! short) and the target twice the distance from the close beyond the close; the size is the money at risk over that
//! distance floored to 0.01. The exit is issued with the entry and stands until it fills. No costs; starts with 100,000.
//!
//! The header's ATR multiple is 0, so its ATR stop is the bar's low (high) plus nothing, which is missing until SMA(14) of
//! the true range has a value. The daily `request.security` high and low only draw lines. shortcut: the New York clock is
//! the United States' rule (second Sunday of March to first of November), written in the file.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Sma, TrueRange};
use vike_marketdata::Bar;
use vike_model::MS_PER_DAY;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::floor_printed;
use vike_tactics::{Rule, RuleDefaults, Tactics, TacticsSizing};
use vike_utility::calendar::{civil_from_days, days_from_civil};

const RISK: f64 = 0.02;
const REWARD: f64 = 2.0;
const HOUR_MS: i64 = 3_600_000;
const MINUTE_MS: i64 = 60_000;

/// The first Sunday on or after a day, 1970-01-01 having been a Thursday.
fn sunday_on_or_after(days: i64) -> i64 {
    days + (3 - days).rem_euclid(7)
}

/// The hours New York is behind UTC at 9:30 in the morning of a UTC day: 4 in summer time, 5 otherwise.
fn behind_utc(days: i64) -> i64 {
    let year = civil_from_days(days).0;
    let starts = sunday_on_or_after(days_from_civil(year, 3, 1)) + 7;
    let ends = sunday_on_or_after(days_from_civil(year, 11, 1));
    if (starts..ends).contains(&days) { 4 } else { 5 }
}

struct S {
    true_range: TrueRange,
    range: Sma,
    last_close: f64,
    in_session: bool,
    high: f64,
    low: f64,
    bull_fired: bool,
    bear_fired: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        true_range: TrueRange::default(),
        range: Sma::new(14.0).expect("a valid length"),
        last_close: f64::NAN,
        in_session: false,
        high: f64::NAN,
        low: f64::NAN,
        bull_fired: false,
        bear_fired: false,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let days = bar.timestamp.div_euclid(MS_PER_DAY);
        let open = days * MS_PER_DAY + (9 * HOUR_MS + 30 * MINUTE_MS) + behind_utc(days) * HOUR_MS;
        let in_session = bar.timestamp >= open && bar.timestamp < open + 15 * MINUTE_MS;
        let past_cutoff = bar.timestamp >= open + 90 * MINUTE_MS;
        let last_close = std::mem::replace(&mut self.last_close, bar.close);
        let average = self.range.next_value(self.true_range.next(bar));
        if !self.in_session && in_session {
            (self.bull_fired, self.bear_fired) = (false, false);
            (self.high, self.low) = (f64::NAN, f64::NAN);
        }
        self.in_session = in_session;
        if in_session {
            self.high = if self.high.is_nan() { bar.high } else { self.high.max(bar.high) };
            self.low = if self.low.is_nan() { bar.low } else { self.low.min(bar.low) };
        }
        let ready = !self.high.is_nan() && !in_session && !past_cutoff;
        let bull = ready && !self.bull_fired && bar.close > self.high && last_close <= self.high;
        self.bull_fired |= bull;
        let bear = ready && !self.bear_fired && bar.close < self.low && last_close >= self.low;
        self.bear_fired |= bear;

        if strategy.position() != 0.0 {
            return;
        }
        // The ATR multiple is 0: the stop is the low (the high) with nothing added, missing while the average is.
        let nothing = average * 0.0;
        if bull {
            let stop = bar.low - nothing;
            let distance = bar.close - stop;
            let target = bar.close + REWARD * distance;
            let qty = size(&strategy.sizing(), strategy.equity(), distance, tick);
            if qty > 0.0 {
                strategy.entry("Long", Long).size(qty);
                strategy.exit("Exit Long").from("Long").take(target).stop(stop);
            }
        }
        if bear {
            let stop = bar.high + nothing;
            let distance = stop - bar.close;
            let target = bar.close - REWARD * distance;
            let qty = size(&strategy.sizing(), strategy.equity(), distance, tick);
            if qty > 0.0 {
                strategy.entry("Short", Short).size(qty);
                strategy.exit("Exit Short").from("Short").take(target).stop(stop);
            }
        }
    }
}

/// The rule's `calculate_position_size`: the money at risk over the stop's distance, floored to the tick; 0 where the distance
/// is no number above 0.
fn size(sizing: &TacticsSizing, equity: f64, distance: f64, tick: f64) -> f64 {
    if distance > 0.0 {
        floor_printed((equity * RISK / distance / tick).floor() * tick, sizing.qty_step())
    } else {
        0.0
    }
}
