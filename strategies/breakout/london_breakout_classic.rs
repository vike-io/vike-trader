// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "London BreakOut Classic" by xsixs, https://www.tradingview.com/script/Fh42LHOM/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2023-11-29, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! The Asian session's range, broken in London. The session is the bars opening from 00:00 to before 06:55 UTC; its high and
//! low are taken when its first bar after it closes. From 07:00 to before 16:00 UTC, the first bar of a day whose low reaches
//! the session's low sells 1 at the market, else one whose high reaches its high buys 1: one entry a day, the day starting
//! with the session. The exit stops at the middle of the range and takes the entry's distance from it again on the other
//! side (a ratio of 1), the target stated once the entry is held. Whatever is held is closed on the 00:00 bar. Only on the
//! bars from 2022-01-01 to before 2024-07-01, and all is closed outside them. No costs; starts with 1,000,000.
//!
//! Read while porting: the session test is on the bar's open time, as the original's session read is. The original's
//! cancel of a resting entry outside the hours cancels nothing (each entry is a market order, filled at the next open
//! before that bar is read), so it is left out, as are the session box and the plots. On a daily or longer chart the
//! original sees no session at all; this file is written for intraday bars. The original imports a published `ta`
//! library but calls none of its functions (`ta.valuewhen` is the language's own), so nothing of it is in this file.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::calendar::{clock, timestamp};
use vike_tactics::helpers::known;
use vike_tactics::{Rule, RuleDefaults, Tactics, TacticsExit};

const LONG: &str = "Enter long";
const SHORT: &str = "Enter short";
/// The target's distance as a multiple of the entry's distance from the stop.
const REWARD_RATIO: f64 = 1.0;

struct S {
    was_in_session: bool,
    session_high: f64,
    session_low: f64,
    range_high: f64,
    range_low: f64,
    entered_long: bool,
    entered_short: bool,
    from: i64,
    to: i64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        was_in_session: false,
        session_high: 0.0,
        session_low: 0.0,
        range_high: f64::NAN,
        range_low: f64::NAN,
        entered_long: false,
        entered_short: false,
        from: timestamp(2022, 1, 1, 0, 0),
        to: timestamp(2024, 7, 1, 0, 0),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let (hour, minute) = clock(bar);
        let minute_of_day = hour * 60 + minute;
        let in_session = minute_of_day < 6 * 60 + 55;
        let session_start = in_session && !self.was_in_session;
        let session_end = self.was_in_session && !in_session;
        self.was_in_session = in_session;
        if session_start {
            self.session_high = bar.high;
            self.session_low = bar.low;
            self.entered_long = false;
            self.entered_short = false;
        } else if in_session {
            self.session_high = self.session_high.max(bar.high);
            self.session_low = self.session_low.min(bar.low);
        }
        if session_end {
            self.range_high = self.session_high;
            self.range_low = self.session_low;
        }
        let stop = (self.range_high - self.range_low) / 2.0 + self.range_low;
        let entry_price = strategy.lots().last().map_or(f64::NAN, |lot| lot.price);
        let take_long = (entry_price - stop) * REWARD_RATIO + entry_price;
        let take_short = entry_price - (stop - entry_price) * REWARD_RATIO;
        let in_dates = bar.timestamp >= self.from && bar.timestamp < self.to;
        let in_hours = (7 * 60..16 * 60).contains(&minute_of_day);
        let open = in_dates && in_hours && !self.entered_long && !self.entered_short;

        if open && bar.low <= self.range_low {
            strategy.entry(SHORT, Short);
            self.entered_short = true;
        }
        legs(strategy.exit("Exit short").from(SHORT), take_short, stop);
        if hour == 0 {
            strategy.close(SHORT);
        }
        if open && !self.entered_short && bar.high >= self.range_high {
            strategy.entry(LONG, Long);
            self.entered_long = true;
        }
        legs(strategy.exit("Exit long").from(LONG), take_long, stop);
        if hour == 0 {
            strategy.close(LONG);
        }
        if !in_dates {
            strategy.close_all();
        }
    }
}

/// The exit's legs the original states: a missing level states no leg.
fn legs(exit: &mut TacticsExit, take: f64, stop: f64) {
    if let Some(take) = known(take) {
        exit.take(take);
    }
    if let Some(stop) = known(stop) {
        exit.stop(stop);
    }
}
