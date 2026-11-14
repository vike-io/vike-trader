// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "My Strategy" by perrycc007, https://www.tradingview.com/script/ZjWK7n8O/
// The original's notice: © stockone1231
// The original's notice: © perrycc007
// Changed: rewritten by hand in Rust for vike.

//! Long 1 on a buy stop at the highest high of the 3 bars before this one, placed while the trend template holds and the range
//! is tight; an unfilled stop rests until it fills or is placed again. The template: the close above SMA(50), SMA(50) above
//! SMA(150) above SMA(200), SMA(200) above itself a bar and 20 bars ago, and SMA(50) not below itself a bar ago. Tight: the
//! 3-bar close range at most 4% of its highest close, or the 3-bar high-low range at most 10% of its highest high and in the
//! lowest 30% of the last 150 bars' ranges (a percent rank). While held, three exits share a stop 10% under the average price
//! and take 25%, 50% and the rest of the entry at 10%, 20% and 30% up. No costs; starts with 100,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_tactics::{Rule, RuleDefaults, Tactics};

use vike_indicators::indicators::{Donchian, PercentRank};
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;

const NAME: &str = "Long";
const SIZE: f64 = 1.0;
const RANK_BARS: f64 = 150.0;
const STOP: f64 = 0.10;
const TARGET: f64 = 0.10;

struct S {
    sma50: IndicatorLine,
    sma150: IndicatorLine,
    sma200: IndicatorLine,
    /// SMA(200) of this bar and the 20 before it, newest first.
    sma200_seen: VecDeque<f64>,
    last_sma50: f64,
    high_low: Donchian,
    closes: Donchian,
    /// vike's `PercentRank`: the percent of the last 150 ranges, this one left out, at most this one.
    rank: PercentRank,
    /// The highest high of the last 3 bars, as the last bar left it.
    last_top: f64,
    entered: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        sma50: IndicatorLine::new("sma", &[50.0]),
        sma150: IndicatorLine::new("sma", &[150.0]),
        sma200: IndicatorLine::new("sma", &[200.0]),
        sma200_seen: VecDeque::with_capacity(22),
        last_sma50: f64::NAN,
        high_low: Donchian::new(3.0).expect("a valid length"),
        closes: Donchian::new(3.0).expect("a valid length"),
        rank: PercentRank::new(RANK_BARS).expect("a valid length"),
        last_top: f64::NAN,
        entered: 0.0,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sma50 = self.sma50.next(bar)[0];
        let sma150 = self.sma150.next(bar)[0];
        let sma200 = self.sma200.next(bar)[0];
        self.sma200_seen.push_front(sma200);
        self.sma200_seen.truncate(21);
        let back = |i: usize| self.sma200_seen.get(i).copied().unwrap_or(f64::NAN);
        let last_sma50 = std::mem::replace(&mut self.last_sma50, sma50);
        let trend = bar.close > sma50
            && sma50 > sma150
            && sma150 > sma200
            && sma200 > back(1)
            && sma200 > back(20)
            && sma50 >= last_sma50;

        let [top, _, bottom] = self.high_low.next(bar);
        let range = (top - bottom) / top * 100.0;
        let [top_close, _, bottom_close] = self.closes.next_hl(bar.close, bar.close);
        let close_range = (top_close - bottom_close) / top_close * 100.0;
        let rank = self.rank.next_value(range);
        let tight = (rank <= 30.0 && range <= 10.0) || close_range <= 4.0;
        let level = std::mem::replace(&mut self.last_top, top);

        let held = strategy.position();
        if held == 0.0 {
            self.entered = 0.0;
        } else if self.entered == 0.0 {
            self.entered = held;
        }
        if trend && tight {
            strategy.entry(NAME, Long).size(SIZE).stop(level);
        }
        if held > 0.0 {
            let cost = strategy.avg_cost();
            let cut = cost * (1.0 - STOP);
            let mut claimed = 0.0;
            for (name, share, steps) in
                [("Close Long1", 0.25, 1.0), ("Close Long2", 0.5, 2.0), ("Close Long3", 1.0, 3.0)]
            {
                let size = (self.entered * share).min(self.entered - claimed);
                claimed += size;
                let take = cost * (1.0 + TARGET * steps);
                strategy.exit(name).from(NAME).size(size).take(take).stop(cut);
            }
        }
    }
}
