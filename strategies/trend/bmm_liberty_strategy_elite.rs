// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Bmm Liberty - Strategy Elite" by denismwabu1, https://www.tradingview.com/script/IyPlUmDS/
// Changed: rewritten by hand in Rust for vike.

//! A Supertrend (ATR(1), factor 2) on the chart's bars, filtered by a vote of six timeframes. Each timeframe votes up when
//! its close is over the EMA(50) of its closes and down when under it (no vote while either is missing): 1, 5 and 15
//! minutes, one hour, four hours and a day. On the bar the Supertrend turns up, with at least three up votes, a long of 10%
//! of the account's worth; on the bar it turns down, with at least three down votes, a short of 10%. Each entry reverses
//! the other side; there is no other exit (the take-profit and stop levels the original draws are lines only, and no order
//! reads them). No costs; starts with 10,000; a margin of 100% of a position's value on each side, as its language version
//! states when the header gives none. The session filter is "All" and the direction "Both", so neither filters anything;
//! the dashboard, the tables, the labels and the alerts are left out.
//!
//! A lower timeframe is an input of the run's own symbol at that bar length (decision 0271), read at the chart bar's close:
//! its close and its EMA over its bars from the rule's first bar on, as they stand after the last of them inside the chart
//! bar. The hour and the four hours are built from the run's own symbol's 30-minute bars from the rule's first bar on
//! (`vike_tactics::higher::HigherTimeframeSourceIntraday`), each read at the last of its bars closed by the chart bar's
//! close; the days are the symbol's daily input, its history before the window included
//! (`vike_tactics::higher::HigherTimeframeSourceCalendar`).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Ema, Supertrend};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::higher::{
    self, HigherTimeframeSourceCalendar, HigherTimeframeSourceIntraday, HigherTimeframeSourceUnseen,
};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{HigherTimeframePeriod, Rule, RuleDefaults, Tactics, TacticsInput};

const ONE_MINUTE: &str = "1m";
const FIVE_MINUTES: &str = "5m";
const FIFTEEN_MINUTES: &str = "15m";
const TREND_LENGTH: f64 = 50.0;
const MINIMUM_VOTES: i32 = 3;
const SHARE: f64 = 10.0;

/// One timeframe's vote: its last close against the EMA of its closes.
struct Vote {
    average: Ema,
    close: f64,
    line: f64,
}

impl Vote {
    fn new() -> Self {
        Vote {
            average: Ema::new(TREND_LENGTH).expect("a valid length"),
            close: f64::NAN,
            line: f64::NAN,
        }
    }

    fn take(&mut self, bar: &Bar) {
        self.close = bar.close;
        self.line = self.average.next(bar);
    }

    /// 1 up, -1 down, 0 while either side is missing or they are equal.
    fn vote(&self) -> i32 {
        if self.close > self.line {
            1
        } else if self.close < self.line {
            -1
        } else {
            0
        }
    }
}

/// A lower timeframe's vote, fed each bar of its input once, from the rule's first bar on.
struct Lower {
    unseen: HigherTimeframeSourceUnseen,
    vote: Vote,
}

impl Lower {
    fn new() -> Self {
        Lower { unseen: HigherTimeframeSourceUnseen::new(), vote: Vote::new() }
    }

    /// The vote after the last bar of `input` closed by now, its bars before `from` left out.
    fn next(&mut self, input: TacticsInput<'_>, from: i64) -> i32 {
        let bars = self.unseen.next(input);
        for bar in &bars[bars.partition_point(|b| b.timestamp < from)..] {
            self.vote.take(bar);
        }
        self.vote.vote()
    }
}

struct S {
    /// The rule's first bar's open: no lower bar before it is fed.
    from: Option<i64>,
    lowers: [Lower; 3],
    hours: HigherTimeframeSourceIntraday,
    hour: Vote,
    fours: HigherTimeframeSourceIntraday,
    four: Vote,
    days: HigherTimeframeSourceCalendar,
    day: Vote,
    supertrend: Supertrend,
    /// The Supertrend's direction on the bar before (1 up, -1 down).
    direction_before: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        from: None,
        lowers: [Lower::new(), Lower::new(), Lower::new()],
        hours: HigherTimeframeSourceIntraday::new(60),
        hour: Vote::new(),
        fours: HigherTimeframeSourceIntraday::new(240),
        four: Vote::new(),
        days: HigherTimeframeSourceCalendar::new(HigherTimeframePeriod::Day),
        day: Vote::new(),
        supertrend: Supertrend::new(1.0, 2.0).expect("valid parameters").with_down_start(true),
        direction_before: f64::NAN,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn inputs(&self) -> Vec<String> {
        [ONE_MINUTE, FIVE_MINUTES, FIFTEEN_MINUTES, higher::HALF_HOUR, higher::DAY]
            .map(str::to_string)
            .to_vec()
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let from = *self.from.get_or_insert(bar.timestamp);
        let mut votes = Vec::with_capacity(6);
        for (lower, name) in self.lowers.iter_mut().zip([ONE_MINUTE, FIVE_MINUTES, FIFTEEN_MINUTES])
        {
            votes.push(lower.next(strategy.input(name), from));
        }
        for hour in self.hours.next(bar, strategy.input(higher::HALF_HOUR)) {
            self.hour.take(&hour);
        }
        for four in self.fours.next(bar, strategy.input(higher::HALF_HOUR)) {
            self.four.take(&four);
        }
        for day in self.days.next(bar, strategy.input(higher::DAY)) {
            self.day.take(&day);
        }
        votes.extend([self.hour.vote(), self.four.vote(), self.day.vote()]);
        let up = votes.iter().filter(|&&v| v == 1).count() as i32;
        let down = votes.iter().filter(|&&v| v == -1).count() as i32;

        let direction = self.supertrend.next(bar)[1];
        let turned_up = direction > self.direction_before;
        let turned_down = direction < self.direction_before;
        self.direction_before = direction;

        let sizing = strategy.sizing();
        if turned_up && up >= MINIMUM_VOTES {
            strategy.entry("Long", Long).size(share_of_worth(&sizing, SHARE, bar.close));
        } else if turned_down && down >= MINIMUM_VOTES {
            strategy.entry("Short", Short).size(share_of_worth(&sizing, SHARE, bar.close));
        }
    }
}
