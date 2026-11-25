// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Qullamaggie Breakout V2" by millerrh, https://www.tradingview.com/script/5bTajWQM/
// The original's notice: © millerrh
// Changed: rewritten by hand in Rust for vike.

//! A breakout of the last swing high, above the daily 50-bar average: long all of the account's worth on a buy stop at the last
//! pivot high (3 bars each side, so confirmed 3 bars after) while that level is over the daily SMA(50). Its stop is the last
//! pivot low, kept where it is once the entry price is at or under the pivot low before it, and replaced by the daily low of
//! the last day that fell under the daily SMA(10) once the position has outlived that day's mark and the last close was over
//! both. Orders fill at the next open or at the stop. 0.1% commission; starts with 100,000, kept in USD. **Does not reproduce
//! its reference** (225 trades, net -35834.542969 on BTCUSDT 30m, 2025-01-01..2026-08-26): this file gives 225 /
//! -35834.595117, 5 cents off on the USD account's conversion, not the rule (booked in USDT it misses by far); the owner
//! accepted those numbers on 2026-11-15.
//!
//! **It looks into the future.** Its daily averages and its daily cross are read with lookahead on, so a bar of a day sees that
//! day's averages, low and cross at their final values (`vike_tactics::higher::HigherTimeframeSourceAhead`, over the run's whole
//! daily series from its first bar). A buy level is held while a position is open. The rule's ADR filter and its pivot drawings
//! are off, and its date range (2019 to 2100) holds on every bar.
//!
//! ```toml
//! [strategy]
//! runs_on = "backtest"
//! why = "looks into the future: reads the day in progress at its final values; a backtest is better than reality"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::ConfirmedPivot;
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::helpers::{BarsSince, fewer, known};
use vike_tactics::higher::HigherTimeframeSourceAhead;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{HigherTimeframePeriod, Rule, RuleDefaults, Tactics};

const SMA_FAST: usize = 10;
const SMA_SLOW: usize = 50;

struct S {
    days: HigherTimeframeSourceAhead,
    /// Every daily close handed out so far, oldest first.
    closes: Vec<f64>,
    /// The day in hand at its final values: its averages, whether its low crossed under the fast one, and the low of the
    /// last such day.
    fast: f64,
    slow: f64,
    crossed: bool,
    level: f64,
    /// The day before's low and fast average.
    last_low: f64,
    last_fast: f64,
    pivots: ConfirmedPivot,
    since_flat: BarsSince,
    since_cross: BarsSince,
    /// The series of the bar before: close, buy level, stop level, pivot low.
    close: f64,
    buy_level: f64,
    stop_level: f64,
    stop_define: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        days: HigherTimeframeSourceAhead::new(HigherTimeframePeriod::Day),
        closes: Vec::new(),
        fast: f64::NAN,
        slow: f64::NAN,
        crossed: false,
        level: f64::NAN,
        last_low: f64::NAN,
        last_fast: f64::NAN,
        pivots: ConfirmedPivot::new(3.0, 3.0).expect("valid lengths"),
        since_flat: BarsSince::default(),
        since_cross: BarsSince::default(),
        close: f64::NAN,
        buy_level: f64::NAN,
        stop_level: f64::NAN,
        stop_define: f64::NAN,
    })
}

impl S {
    /// The mean of the last `n` daily closes, NaN before there are `n`.
    fn mean(&self, n: usize) -> f64 {
        let c = &self.closes;
        if c.len() < n {
            return f64::NAN;
        }
        let mut sum = 0.0;
        for x in &c[c.len() - n..] {
            sum += x;
        }
        sum / n as f64
    }

    /// A day handed out, at its final values.
    fn day(&mut self, d: &Bar) {
        self.closes.push(d.close);
        let (fast, slow) = (self.mean(SMA_FAST), self.mean(SMA_SLOW));
        self.crossed = d.low < fast && self.last_low >= self.last_fast;
        if self.crossed {
            self.level = d.low;
        }
        (self.fast, self.slow) = (fast, slow);
        (self.last_low, self.last_fast) = (d.low, fast);
    }
}

/// The price a buy stop at `level` is sized from: the level on the tick, or this close when the close is already past it.
fn sized_at(strategy: &Tactics<'_>, level: f64, close: f64) -> f64 {
    let level = strategy.snap_stop(Long, level);
    if close >= level { close } else { level }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.1), ..RuleDefaults::PLAIN }
    }

    fn inputs(&self) -> Vec<String> {
        vec![self.days.input().to_string()]
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        for d in self.days.next(bar, strategy) {
            self.day(&d);
        }
        let [_, _, pivot_high, pivot_low] = self.pivots.next(bar);
        let in_position = strategy.position() > 0.0;
        let buy_conditions = pivot_high > self.slow;
        let buy_level = if in_position { self.buy_level } else { pivot_high };

        let in_profit = strategy.avg_cost() <= self.stop_define;
        let stop_level = if in_position && !in_profit {
            pivot_low
        } else if in_position {
            self.stop_level
        } else {
            pivot_low
        };
        let flat_for = self.since_flat.step(!in_position);
        let cross_for = self.since_cross.step(self.crossed);
        let trail_stop =
            if in_position && fewer(cross_for, flat_for) { self.level } else { f64::NAN };
        let stop = if stop_level > trail_stop {
            stop_level
        } else if self.close > trail_stop && self.close > self.fast {
            trail_stop
        } else {
            stop_level
        };

        if buy_conditions {
            let size = share_of_worth(&sizing, 100.0, sized_at(strategy, buy_level, bar.close));
            strategy.entry("Buy", Long).size(size).stop(buy_level);
        }
        let exit = strategy.exit("Sell").from("Buy");
        if let Some(cut) = known(stop) {
            exit.stop(cut);
        }

        self.close = bar.close;
        self.buy_level = buy_level;
        self.stop_level = stop_level;
        self.stop_define = pivot_low;
    }
}
