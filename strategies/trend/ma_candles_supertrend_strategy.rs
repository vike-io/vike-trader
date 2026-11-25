// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "MA Candles Supertrend Strategy" by Trendoscope, https://www.tradingview.com/script/3o1JCZQL/
// The original's notice: © HeWhoMustNotBeNamed
// Changed: rewritten by hand in Rust for vike.

//! A SuperTrend drawn on candles of Wilder averages (length 20) of the open, high, low and close. Its true range is the span
//! from the lower of the average low and the last average close to the higher of the average high and it, averaged over 30 by
//! Wilder; the stops step with the average low and high (wicks on), and the trend turns down when the average low closes under the
//! last long stop and up when the average high passes the last short stop. Long all of the account's worth when the trend has
//! just been up and the close lies over 80% of last year's high or over 170% of its low; closed when the trend is down and either
//! the bar before was down too with a lower close, or the close is under the long stop. Longs only (`allow_entry_in`): a short
//! entry could only close a long, which the same bar's close does. 0.01% commission; starts with 20,000.
//!
//! "Last year" is the calendar year before the bar's at its final values (the original's 12-month call reads `high[1]` with
//! lookahead): its high and low over the closed months of the run's own symbol, each month built from its daily bars
//! (`vike_tactics::higher::HigherTimeframeSourceCalendar`). Every month of that year has closed by the year's first bar.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::HashMap;
use vike_indicators::indicators::Smma;
use vike_indicators::strategy_kit::{max, min};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::higher::{self, HigherTimeframeSourceCalendar};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{HigherTimeframePeriod, Rule, RuleDefaults, Tactics};
use vike_utility::calendar::civil_from_days;

const NAME: &str = "Long";
const LOOKBACK: usize = 20;
const ATR_LENGTH: usize = 30;
const D_THRESHOLD: f64 = 0.2;
const R_THRESHOLD: f64 = 0.7;
const DAY_MS: i64 = 86_400_000;

/// The rule's Wilder average, a missing value skipped.
fn wilder(length: usize) -> Smma {
    Smma::new(length as f64).expect("a valid length").with_skip_nan(true)
}

/// The UTC calendar year of an instant.
fn year_of(ts: i64) -> i64 {
    civil_from_days(ts.div_euclid(DAY_MS)).0
}

struct S {
    months: HigherTimeframeSourceCalendar,
    /// Each calendar year's `(high, low)` from the months closed so far.
    years: HashMap<i64, (f64, f64)>,
    high: Smma,
    low: Smma,
    close: Smma,
    range: Smma,
    /// The last bar's average candle, stops, trend and close.
    last: Option<Last>,
}

#[derive(Clone, Copy)]
struct Last {
    o_close: f64,
    o_low: f64,
    o_high: f64,
    long_stop: f64,
    short_stop: f64,
    dir: i8,
    close: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        months: HigherTimeframeSourceCalendar::new(HigherTimeframePeriod::Month),
        years: HashMap::new(),
        high: wilder(LOOKBACK),
        low: wilder(LOOKBACK),
        close: wilder(LOOKBACK),
        range: wilder(ATR_LENGTH),
        last: None,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.01), ..RuleDefaults::PLAIN }
    }

    fn inputs(&self) -> Vec<String> {
        vec![higher::DAY.to_string()]
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        for month in self.months.next(bar, strategy.input(higher::DAY)) {
            let year =
                self.years.entry(year_of(month.timestamp)).or_insert((month.high, month.low));
            *year = (year.0.max(month.high), year.1.min(month.low));
        }
        let (year_high, year_low) =
            self.years.get(&(year_of(bar.timestamp) - 1)).copied().unwrap_or((f64::NAN, f64::NAN));
        let year_high_condition = bar.close > year_high * (1.0 - D_THRESHOLD)
            || bar.close > year_low * (1.0 + R_THRESHOLD);

        let (o_high, o_low, o_close) = (
            self.high.next_value(bar.high),
            self.low.next_value(bar.low),
            self.close.next_value(bar.close),
        );
        let previous_close = self.last.map_or(f64::NAN, |l| l.o_close);
        let range = max(o_high, previous_close) - min(o_low, previous_close);
        let atr = self.range.next_value(range);

        let (previous_long, previous_short) =
            self.last.map_or((f64::NAN, f64::NAN), |l| (l.long_stop, l.short_stop));
        // Left: no `Supertrend` form is this one: the stops carry on the last candle's low and high and turn on this one's.
        let mut long_stop = o_close - atr;
        let long_previous = if previous_long.is_nan() { long_stop } else { previous_long };
        if self.last.map_or(f64::NAN, |l| l.o_low) > long_previous {
            long_stop = max(long_stop, long_previous);
        }
        let mut short_stop = o_close + atr;
        let short_previous = if previous_short.is_nan() { short_stop } else { previous_short };
        if self.last.map_or(f64::NAN, |l| l.o_high) < short_previous {
            short_stop = min(short_stop, short_previous);
        }
        let before = self.last.map_or(1, |l| l.dir);
        let dir = if before == -1 && o_high > short_previous {
            1
        } else if before == 1 && o_low < long_previous {
            -1
        } else {
            before
        };
        let prior_dir = self.last.map(|l| l.dir);
        let prior_close = self.last.map_or(f64::NAN, |l| l.close);
        let exit_long = dir == -1
            && (prior_dir == Some(-1) && bar.close < prior_close || bar.close < long_stop);
        let exit_short =
            dir == 1 && (prior_dir == Some(1) && bar.close > prior_close || bar.close > short_stop);
        self.last =
            Some(Last { o_close, o_low, o_high, long_stop, short_stop, dir, close: bar.close });

        if exit_short && year_high_condition {
            strategy.entry(NAME, Long).size(share_of_worth(&sizing, 100.0, bar.close));
        }
        if exit_long {
            strategy.close(NAME);
        }
    }
}
