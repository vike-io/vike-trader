// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "MACD ASHI BARS Wıth SL-TP" by ridvansozen1, https://www.tradingview.com/script/YfPnKE22/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2021-03-19, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! A MACD of the Heikin-Ashi close (`(open + high + low + close) / 4`) built from double SMAs: each line is `2 * sma - sma(sma)`,
//! 12 bars fast and 25 slow, the signal line the SMA(15) of their difference. When the difference less the signal crosses up
//! through 0 on a bar of the years 2020 to 2022 (a 31st day excluded), a long stop at that bar's close; when it crosses down, a
//! short stop there. Each rests for the next bar alone, pulled on a bar that does not cross again, and the two are one group,
//! so a fill pulls the other. Each takes all of the account's worth, the opposite side reversing; one entry a side. Every bar
//! the held entry gets a target 10% and a stop 3% of this bar's close from its fill, one cancelling the other; an entry still
//! to fill gets them from its fill on, with the distances of its signal bar. 0.1% commission, 1 tick of slippage; starts with
//! 100,000.
//!
//! A stop's size spends the account's worth at its level, this close, moved by the slippage. The plot of the histogram and the
//! lines is left out.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Sma;
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::MS_PER_DAY;
use vike_model::OrderSide::{self, Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};
use vike_utility::calendar::civil_from_days;

const LONG: &str = "LONG";
const SHORT: &str = "SHORT";
const GROUP: &str = "TREND";
const PROFIT_PERCENT: f64 = 10.0;
const LOSS_PERCENT: f64 = 3.0;
const FIRST_YEAR: i64 = 2020;
const LAST_YEAR: i64 = 2022;
/// The original's day filter is `dayofmonth < 31`.
const DAY_BEFORE: u32 = 31;

fn sma(length: f64) -> Sma {
    Sma::new(length).expect("a valid length")
}

/// `2 * sma - sma(sma)` of a length, the second average fed from the first's first value on.
struct DoubleAverage {
    first: Sma,
    second: Sma,
}

impl DoubleAverage {
    fn new(length: f64) -> Self {
        DoubleAverage { first: sma(length), second: sma(length) }
    }

    fn next(&mut self, x: f64) -> f64 {
        let first = self.first.next_value(x);
        if first.is_nan() {
            return f64::NAN;
        }
        2.0 * first - self.second.next_value(first)
    }
}

struct S {
    fast: DoubleAverage,
    slow: DoubleAverage,
    signal: Sma,
    cross: Cross,
    /// The `(target, stop)` distances the entry placed this bar takes from its fill.
    coming: Option<(f64, f64)>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: DoubleAverage::new(12.0),
        slow: DoubleAverage::new(25.0),
        signal: sma(15.0),
        cross: Cross::new(),
        coming: None,
    })
}

impl S {
    /// `(target, stop)` distances from an entry's fill, as this close makes them, counted in ticks of `tick`.
    fn distances(close: f64, tick: f64) -> (f64, f64) {
        let ticks = |percent: f64| close * (percent / 100.0) / tick;
        (ticks(PROFIT_PERCENT) * tick, ticks(LOSS_PERCENT) * tick)
    }

    /// The exit of the entry `name` on `side`, filled at `price`.
    fn protect(
        strategy: &mut Tactics<'_>,
        name: &str,
        side: OrderSide,
        price: f64,
        (take, cut): (f64, f64),
    ) {
        let up = f64::from(side);
        strategy
            .exit(&format!("{name} EXIT"))
            .from(name)
            .take(price + up * take)
            .stop(price - up * cut);
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.1), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        self.coming = None;
        let source = (bar.open + bar.high + bar.low + bar.close) / 4.0;
        let macd = self.fast.next(source) - self.slow.next(source);
        let signal = if macd.is_nan() { f64::NAN } else { self.signal.next_value(macd) };
        let cross = self.cross.step(macd - signal, 0.0);
        let Some(tick) = strategy.tick() else { return };
        let (year, _, day) = civil_from_days(bar.timestamp.div_euclid(MS_PER_DAY));
        let dated = (FIRST_YEAR..=LAST_YEAR).contains(&year) && day < DAY_BEFORE;
        let sizing = strategy.sizing();
        let distances = Self::distances(bar.close, tick);
        for (name, side, wanted) in [
            (LONG, Long, matches!(cross, Signal::Long)),
            (SHORT, Short, matches!(cross, Signal::Short)),
        ] {
            if wanted && dated {
                let size = share_of_worth(&sizing, 100.0, sizing.slipped(side, bar.close));
                strategy.entry(name, side).size(size).stop(bar.close).group(GROUP);
                self.coming = Some(distances);
            } else {
                strategy.cancel(name);
            }
        }
        let held: Vec<(String, OrderSide, f64)> =
            strategy.bindings().iter().map(|b| (b.name.clone(), b.side, b.price)).collect();
        for (name, side, price) in held {
            Self::protect(strategy, &name, side, price, distances);
        }
    }

    /// The entry placed on the bar before gets its exit from its fill on.
    fn after_fill(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(distances) = self.coming else { return };
        let Some(b) = strategy.bindings().last().filter(|b| b.entry_ms == bar.timestamp) else {
            return;
        };
        let (name, side, price) = (b.name.clone(), b.side, b.price);
        self.coming = None;
        Self::protect(strategy, &name, side, price, distances);
    }
}
