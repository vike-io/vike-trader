// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "ATR Trend Strategy with Moving Average | Fixed TP/SL version" by lahonglam, https://www.tradingview.com/script/fM3YQz8i/
// The original's notice: © lahonglam
// Changed: rewritten by hand in Rust for vike.

//! Long 10% of the account's worth when a trend turns up, short 10% when it turns down, from flat only. The trend is read off
//! bands of 3 times the RMA(7) of the true range about EMA(100), both of the bar before: the trend is up from the second
//! bar in a row whose high is over the upper band (down: whose low is under the lower band), unless it already is. Once held,
//! a target and a stop sit at the cost plus or minus the 70th percentile of what the finished trends so far gave: the target
//! the favourable excursion, the stop the adverse one, each as a percent of the trend's first close, taken over the last 500
//! trends by linear interpolation of the sorted list; neither stands until a trend has finished. They are stated again on
//! every bar, so they move as trends finish. 0.04% commission and 2 ticks of slippage; starts with 10,000, kept in USD.
//!
//! The rule's library import is not used by any of this.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! stacked_closes = true
//! ```

use vike_indicators::indicators::{Atr, Ema};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::helpers::{BarsSince, known};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME_LONG: &str = "Long";
const NAME_SHORT: &str = "Short";
const CONFIRM_BARS: usize = 2;
const BAND: f64 = 3.0;
const SAMPLES: usize = 500;

struct S {
    ma: Ema,
    atr: Atr,
    /// The average and the range of the bar before.
    last: (f64, f64),
    up: BarsSince,
    down: BarsSince,
    trend: i32,
    start: f64,
    highest: f64,
    lowest: f64,
    favourable: f64,
    adverse: f64,
    bars: u32,
    favourable_list: Vec<f64>,
    adverse_list: Vec<f64>,
    /// The 70th percentiles of the two lists.
    take: f64,
    cut: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        ma: Ema::new(100.0).expect("a valid length"),
        atr: Atr::new(7.0).expect("a valid length"),
        last: (f64::NAN, f64::NAN),
        up: BarsSince::default(),
        down: BarsSince::default(),
        trend: 0,
        start: f64::NAN,
        highest: f64::NAN,
        lowest: f64::NAN,
        favourable: f64::NAN,
        adverse: f64::NAN,
        bars: 0,
        favourable_list: Vec::new(),
        adverse_list: Vec::new(),
        take: f64::NAN,
        cut: f64::NAN,
    })
}

/// The reference's percentile: linear between the two sorted values the fractional index lies between, missing when empty.
fn percentile(list: &[f64], p: f64) -> f64 {
    if list.is_empty() {
        return f64::NAN;
    }
    let mut sorted = list.to_vec();
    sorted.sort_by(f64::total_cmp);
    let index = (sorted.len() - 1) as f64 * p;
    let (lower, upper) = (index.floor(), index.ceil());
    let (a, b) = (sorted[lower as usize], sorted[upper as usize]);
    a + (b - a) * (index - lower)
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.04), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let (high, low, close) = (bar.high, bar.low, bar.close);
        let (ma, atr) = std::mem::replace(&mut self.last, (self.ma.next(bar), self.atr.next(bar)));
        let upper = ma + atr * BAND;
        let lower = ma - atr * BAND;
        // A band that does not exist yet is not broken, so the bar counts as one that was not (a NaN compares false).
        let (up_raw, down_raw) = (high > upper, low < lower);
        let up_confirmed = self.up.step(!up_raw) == Some(CONFIRM_BARS);
        let down_confirmed = self.down.step(!down_raw) == Some(CONFIRM_BARS);

        let before = self.trend;
        let mut turn = 0;
        if up_confirmed && self.trend != 1 {
            self.trend = 1;
            turn = 1;
        }
        if down_confirmed && self.trend != -1 {
            self.trend = -1;
            turn = -1;
        }
        let ended = before != 0 && self.trend != before;
        let started = self.trend != 0 && self.trend != before;
        if ended {
            if self.favourable / self.start > 0.0 {
                self.favourable_list.push(self.favourable / self.start * 100.0);
            }
            if self.adverse / self.start > 0.0 {
                self.adverse_list.push(self.adverse / self.start * 100.0);
            }
            for list in [&mut self.favourable_list, &mut self.adverse_list] {
                if list.len() > SAMPLES {
                    list.remove(0);
                }
            }
            self.take = percentile(&self.favourable_list, 0.7);
            self.cut = percentile(&self.adverse_list, 0.7);
        }
        if started {
            self.start = close;
            self.highest = close;
            self.lowest = close;
            self.favourable = 0.0;
            self.adverse = 0.0;
            self.bars = 0;
        }
        self.bars = if self.trend != 0 { self.bars + 1 } else { 0 };
        if self.bars > 1 {
            self.highest = self.highest.max(high);
            self.lowest = self.lowest.min(low);
            if self.trend == 1 {
                self.favourable = self.highest - self.start;
                self.adverse = self.start - self.lowest;
            } else {
                self.favourable = self.start - self.lowest;
                self.adverse = self.highest - self.start;
            }
        }

        let held = strategy.position();
        if held == 0.0 {
            if turn == 1 {
                strategy.entry(NAME_LONG, Long).size(share_of_worth(&sizing, 10.0, close));
            }
            if turn == -1 {
                strategy.entry(NAME_SHORT, Short).size(share_of_worth(&sizing, 10.0, close));
            }
        } else {
            let cost = strategy.avg_cost();
            let side = held.signum();
            let take = known(cost * (1.0 + side * self.take / 100.0));
            let cut = known(cost * (1.0 - side * self.cut / 100.0));
            let lots = if held > 0.0 { NAME_LONG } else { NAME_SHORT };
            let exit = strategy.exit("Exit").from(lots);
            if let Some(take) = take {
                exit.take(take);
            }
            if let Some(cut) = cut {
                exit.stop(cut);
            }
        }
    }
}
