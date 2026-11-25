// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "EMA RSI ATR Hidden Div Strat" by TradingStrategyCheck, https://www.tradingview.com/script/itKnLY6I/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2021-06-06, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! A hidden divergence of RSI(9) of the close: on a bar that is the lowest RSI of the last five (a pivot low confirmed at
//! once), the RSI under the one at the pivot low before, which came 6 or 7 bars earlier, while the low is above that pivot's
//! low; the mirror on the highs. Long all of the account's worth at the next open, when flat, if a hidden bullish
//! divergence came on one of the last five bars, EMA(50) is over EMA(150), the closes of the last three bars were over
//! EMA(50) and this one is, and the stochastic %K (3, smoothed 4) crosses up through its 3-bar average; short on the mirror.
//! On the fill bar a target 9 ATR(11) and a stop 5 ATR(11) from the fill are set, and they hold for the trade. No costs;
//! starts with 5,000.
//!
//! The rule writes each EMA test as `close > ema1 and ema2`, so the slow EMA only has to exist and not be 0; the first
//! close of the short's test is two bars back on both of its first two terms. Kept as written.
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
use vike_tactics::helpers::truthy;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

use vike_indicators::indicators::Stochastic;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;

const UP: &str = "long";
const DOWN: &str = "short";
const PIVOT_BARS: usize = 5;
const RANGE: (usize, usize) = (5, 6);
const PROFIT_ATRS: f64 = 9.0;
const STOP_ATRS: f64 = 5.0;

/// One side of the divergence: the pivot test, the two latest pivots' `(rsi, price)`, and the bars since the pivot before.
#[derive(Default)]
struct Divergence {
    /// RSI values since the last missing one, newest last, at most five.
    window: VecDeque<f64>,
    pivots: VecDeque<(f64, f64)>,
    found_before: bool,
    since: Option<usize>,
    /// The last five bars' results, newest first.
    recent: VecDeque<bool>,
}

impl Divergence {
    /// `low` true for pivot lows. `price` is the low or the high. Gives whether a hidden divergence fired on any of the five
    /// bars before this one.
    fn step(&mut self, index: usize, rsi: f64, price: f64, low: bool) -> bool {
        let found = if rsi.is_nan() {
            self.window.clear();
            false
        } else {
            self.window.push_back(rsi);
            if self.window.len() > PIVOT_BARS {
                self.window.pop_front();
            }
            let extreme = |w: &VecDeque<f64>| {
                if low { w.iter().all(|&x| rsi <= x) } else { w.iter().all(|&x| rsi >= x) }
            };
            index + 1 >= PIVOT_BARS && extreme(&self.window)
        };
        self.since = if self.found_before { Some(0) } else { self.since.map(|n| n + 1) };
        self.found_before = found;
        let in_range = self.since.is_some_and(|n| (RANGE.0..=RANGE.1).contains(&n));
        if found {
            self.pivots.push_front((rsi, price));
            self.pivots.truncate(2);
        }
        let hidden = found
            && self.pivots.get(1).is_some_and(|&(rsi_before, price_before)| {
                if low {
                    rsi < rsi_before && price > price_before
                } else {
                    rsi > rsi_before && price < price_before
                }
            })
            && in_range;
        let any = self.recent.iter().any(|&h| h);
        self.recent.push_front(hidden);
        self.recent.truncate(5);
        any
    }
}

struct S {
    index: usize,
    fast: IndicatorLine,
    slow: IndicatorLine,
    rsi: IndicatorLine,
    atr: IndicatorLine,
    stoch: Stochastic,
    bull: Divergence,
    bear: Divergence,
    /// `(close, fast, slow, k, d)` of the last three bars, newest first.
    past: VecDeque<(f64, f64, f64, f64, f64)>,
    held: f64,
    /// The bracket of the trade held, fixed on its fill bar.
    bracket: Option<(f64, f64)>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        index: 0,
        fast: IndicatorLine::new("ema", &[50.0]),
        slow: IndicatorLine::new("ema", &[150.0]),
        rsi: IndicatorLine::new("rsi", &[9.0]),
        atr: IndicatorLine::new("atr", &[11.0]),
        stoch: Stochastic::new(3.0, 4.0, 3.0).expect("valid lengths").with_fresh_sum(true),
        bull: Divergence::default(),
        bear: Divergence::default(),
        past: VecDeque::with_capacity(4),
        held: 0.0,
        bracket: None,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let index = self.index;
        self.index += 1;
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        let rsi = self.rsi.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        let [k, d] = self.stoch.next(bar);
        let bull = self.bull.step(index, rsi, bar.low, true);
        let bear = self.bear.step(index, rsi, bar.high, false);

        let past = |i: usize| {
            self.past.get(i - 1).copied().unwrap_or((
                f64::NAN,
                f64::NAN,
                f64::NAN,
                f64::NAN,
                f64::NAN,
            ))
        };
        let (c1, f1, s1, k1, d1) = past(1);
        let (c2, f2, s2, _, _) = past(2);
        let (c3, f3, s3, _, _) = past(3);
        let up = c1 > f1 && truthy(s1) && c2 > f2 && truthy(s2) && c3 > f3 && truthy(s3);
        let down = c2 < f1 && truthy(s1) && c2 < f2 && truthy(s2) && c3 < f3 && truthy(s3);
        let cross_up = k > d && k1 <= d1;
        let cross_down = k < d && k1 >= d1;
        let buy = fast > slow && up && cross_up && bull && bar.close > fast && truthy(slow);
        let sell = fast < slow && down && cross_down && bear && bar.close < fast && truthy(slow);
        self.past.push_front((bar.close, fast, slow, k, d));
        self.past.truncate(3);

        let held = strategy.position();
        let flat = strategy.lots().is_empty();
        if flat && buy {
            strategy.entry(UP, Long).size(share_of_worth(&sizing, 100.0, bar.close));
        }
        if flat && sell {
            strategy.entry(DOWN, Short).size(share_of_worth(&sizing, 100.0, bar.close));
        }
        if held == 0.0 {
            self.bracket = None;
        } else if held != self.held || self.bracket.is_none() {
            let toward = held.signum();
            let cost = strategy.avg_cost();
            self.bracket =
                Some((cost + toward * atr * PROFIT_ATRS, cost - toward * atr * STOP_ATRS));
        }
        self.held = held;
        if let Some((take, cut)) = self.bracket {
            let (name, lots) =
                if held > 0.0 { ("long bracket", UP) } else { ("short bracket", DOWN) };
            strategy.exit(name).from(lots).take(take).stop(cut);
        }
    }
}
