// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Strategy Template - V2" by Trendoscope, https://www.tradingview.com/script/nSgePqla/
// The original's notice: © HeWhoMustNotBeNamed
// Changed: rewritten by hand in Rust for vike.

//! A 40-bar Donchian channel of the chart (highest high and lowest low), and the same channel of 40 weekly bars. Over the
//! weekly channel's midpoint the bias is long when the chart channel's low is over it; the bias is short when the chart
//! channel's high is under it. With a long bias a short is closed and its stop dropped, and a long rests as a stop at the
//! chart channel's high; the short bias is the mirror at its low. Whatever is held keeps a stop at the opposite side of the
//! chart channel (a long at the low, a short at the high). All of the account's worth per entry; 0.11% commission; starts
//! with 20,000. Pyramiding is 1; the rule's date range holds on every bar here.
//!
//! The weekly channel is read with lookahead on and one bar back, so on every chart bar of a week it is the channel as of the
//! week before, last bar of the week included: the channel of the last week that ended by this bar's open, never the week in
//! progress. The weeks are built from the symbol's daily input, its history before the window included
//! (`vike_tactics::higher::HigherTimeframeSourceCalendar`).
//!
//! A stop's size spends the account's worth at its level on the grid, or at this close when the close is already through it.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Donchian;
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::higher::{self, HigherTimeframeSourceCalendar};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{HigherTimeframePeriod, Rule, RuleDefaults, Tactics};

const LENGTH: f64 = 40.0;

struct S {
    channel: Donchian,
    weeks: HigherTimeframeSourceCalendar,
    weekly: Donchian,
    /// `(week's start, high, low)` of the weekly channel as of the last week closed, and as of the week before it.
    last: (i64, f64, f64),
    before: (i64, f64, f64),
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        channel: Donchian::new(LENGTH).expect("a valid length"),
        weeks: HigherTimeframeSourceCalendar::new(HigherTimeframePeriod::Week),
        weekly: Donchian::new(LENGTH).expect("a valid length"),
        last: (i64::MIN, f64::NAN, f64::NAN),
        before: (i64::MIN, f64::NAN, f64::NAN),
    })
}

/// The price a stop entry at `level` is sized from.
fn sized_at(strategy: &Tactics<'_>, side: OrderSide, level: f64, close: f64) -> f64 {
    let level = strategy.snap_stop(side, level);
    if (side == Long && level <= close) || (side == Short && level >= close) {
        close
    } else {
        level
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.11), ..RuleDefaults::PLAIN }
    }

    fn inputs(&self) -> Vec<String> {
        vec![higher::DAY.to_string()]
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        for week in self.weeks.next(bar, strategy.input(higher::DAY)) {
            let [high, _, low] = self.weekly.next_hl(week.high, week.low);
            self.before = std::mem::replace(&mut self.last, (week.timestamp, high, low));
        }
        // On a week's last bar that week has just closed: the week before is the one before it.
        let (_, htf_high, htf_low) =
            if HigherTimeframePeriod::Week.end(self.last.0) <= bar.timestamp {
                self.last
            } else {
                self.before
            };
        let [top, _, bottom] = self.channel.next_hl(bar.high, bar.low);
        let hmid = (htf_high + htf_low) / 2.0;
        let long_bias = bottom > hmid;
        let short_bias = top < hmid;
        if long_bias {
            strategy.cancel("Short");
            strategy.close("Short");
            let size = share_of_worth(&sizing, 100.0, sized_at(strategy, Long, top, bar.close));
            strategy.entry("Long", Long).size(size).stop(top);
        }
        if short_bias {
            strategy.cancel("Long");
            strategy.close("Long");
            let size = share_of_worth(&sizing, 100.0, sized_at(strategy, Short, bottom, bar.close));
            strategy.entry("Short", Short).size(size).stop(bottom);
        }
        if strategy.position() > 0.0 {
            strategy.exit("StopLong").from("Long").stop(bottom);
        }
        if strategy.position() < 0.0 {
            strategy.exit("StopShort").from("Short").stop(top);
        }
    }
}
