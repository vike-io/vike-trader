// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "All in One Strategy" by Solutions1978, https://www.tradingview.com/script/6vHRkdsf/
// Changed: rewritten by hand in Rust for vike.

//! RSI(14) reversals at a dip or a peak, between 2021-06-22 05:00 and 2021-12-31 06:00 UTC, all of the account's worth, orders
//! filling at the signal bar's close. A dip: the close is under the open five bars back, the running high is over it and the
//! running low under it, the running high over the bar before's open and the running low under it, and that bar closed down;
//! a peak is the mirror with the running low over the open five bars back. The running high is this bar's high when it is
//! over the bar before's, else it stays; the running low likewise. When flat, long when the RSI is under 35 at a dip; short when
//! it is over 62 at a peak with its EMA(14) under it and the stochastic's %D (14, 3, 3) under its %K. While held, a stop 10%
//! and a target 20% from the entry; the long closes on the short's signal, the short on the long's. 0.18% commission; starts
//! with 1,000.
//!
//! The rule halts for good after two losing days in a row, and at a drawdown of 10% of equity: every order is cancelled and the
//! position closed at the next open, the reference's measured rule (closing at the halting bar's close instead gives 68.502785,
//! 0.075 over the reference's 68.427544). A day's result is its last bar's marked equity against the day before's, a day with
//! no change ending a run of losing days. The drawdown is the larger of the capital and the realized equity so far less the
//! equity marked now; on this window it never halts (the run is the same without it).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;

use vike_indicators::indicators::{Ema, Rsi, Stochastic};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::calendar::timestamp;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const DAY_MS: i64 = 86_400_000;
const CAPITAL: f64 = 1_000.0;
const STOP: f64 = 0.10;
const TARGET: f64 = 0.20;
const LOSING_DAYS: u32 = 2;
const MAX_DRAWDOWN: f64 = 10.0;
const LOOKBACK: usize = 5;

struct S {
    rsi: Rsi,
    rsi_ema: Ema,
    stoch: Stochastic,
    /// The opens of the last `LOOKBACK + 1` bars, newest last, and the bar before's close, high and low.
    opens: VecDeque<f64>,
    close_before: f64,
    high_before: f64,
    low_before: f64,
    /// The running high and low.
    highest: f64,
    lowest: f64,
    previous_ts: Option<i64>,
    /// The marked equity at the last day's end, the losing days in a row, the realized peak, and whether the run is halted.
    day_end: f64,
    losing: u32,
    peak: f64,
    halted: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        rsi: Rsi::new(14.0).expect("a valid length"),
        rsi_ema: Ema::new(14.0).expect("a valid length").with_skip_nan(true),
        stoch: Stochastic::new(14.0, 3.0, 3.0).expect("valid lengths"),
        opens: VecDeque::new(),
        close_before: f64::NAN,
        high_before: f64::NAN,
        low_before: f64::NAN,
        highest: f64::NAN,
        lowest: f64::NAN,
        previous_ts: None,
        day_end: f64::NAN,
        losing: 0,
        peak: CAPITAL,
        halted: false,
    })
}

impl S {
    /// The two halts, judged after the bar: on its day's last bar the day's result, then the drawdown.
    fn risk(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let equity = strategy.equity();
        let step = self.previous_ts.map_or(0, |p| bar.timestamp - p);
        self.previous_ts = Some(bar.timestamp);
        let last_of_day = step > 0
            && (bar.timestamp + step).div_euclid(DAY_MS) != bar.timestamp.div_euclid(DAY_MS);
        if last_of_day {
            if equity < self.day_end {
                self.losing += 1;
            } else if !self.day_end.is_nan() {
                self.losing = 0;
            }
            self.day_end = equity;
        }
        if self.halted {
            return;
        }
        self.peak = self.peak.max(CAPITAL + strategy.closed_net().unwrap_or(0.0));
        let drawdown = self.peak - equity >= self.peak * MAX_DRAWDOWN / 100.0;
        if self.losing >= LOSING_DAYS || drawdown {
            self.halted = true;
            strategy.cancel_all();
            strategy.close_all();
        }
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.18), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let rsi = self.rsi.next(bar);
        let rsi_ema = self.rsi_ema.next_value(rsi);
        let [k, d] = self.stoch.next(bar);
        self.opens.push_back(bar.open);
        if self.opens.len() > LOOKBACK + 1 {
            self.opens.pop_front();
        }
        if bar.high > self.high_before {
            self.highest = bar.high;
        }
        if bar.low < self.low_before {
            self.lowest = bar.low;
        }
        let first = self.opens.front().copied().filter(|_| self.opens.len() > LOOKBACK);
        let first = first.unwrap_or(f64::NAN);
        let previous_open = self.opens.iter().rev().nth(1).copied().unwrap_or(f64::NAN);
        let percent = |a: f64, b: f64| (a - b) / b * 100.0;
        let overall = percent(bar.close, first);
        let to_high = percent(self.highest, first);
        let to_low = percent(self.lowest, first);
        let last_high = percent(self.highest, previous_open);
        let last_low = percent(self.lowest, previous_open);
        let last_bar = percent(self.close_before, previous_open);
        (self.close_before, self.high_before, self.low_before) = (bar.close, bar.high, bar.low);
        let shared = to_high > 0.0 && last_high > 0.0 && last_low < 0.0;
        let true_dip = shared && overall < 0.0 && to_low < 0.0 && last_bar < 0.0;
        let true_peak = shared && overall > 0.0 && to_low > 0.0 && last_bar > 0.0;
        let window =
            (timestamp(2021, 6, 22, 5, 0)..=timestamp(2021, 12, 31, 6, 0)).contains(&bar.timestamp);
        let oversold = rsi < 35.0 && true_dip && window;
        let overbought = rsi > 62.0 && true_peak && window;
        let other_short = rsi_ema < rsi && d < k;

        self.risk(strategy, bar);
        if self.halted {
            return;
        }
        let held = strategy.position();
        if held == 0.0 {
            let size = share_of_worth(&sizing, 100.0, bar.close);
            if oversold {
                strategy.entry("LONG", Long).size(size).here();
            } else if other_short && overbought {
                strategy.entry("SHORT", Short).size(size).here();
            }
        }
        let cost = strategy.avg_cost();
        if held > 0.0 {
            let exit = strategy.exit("Exit Long").from("LONG");
            exit.stop(cost * (1.0 - STOP)).take(cost * (1.0 + TARGET));
        }
        if held < 0.0 {
            let exit = strategy.exit("Exit Short").from("SHORT");
            exit.stop(cost * (1.0 + STOP)).take(cost * (1.0 - TARGET));
        }
        if held > 0.0 && other_short && overbought {
            strategy.close("LONG").here();
        }
        if oversold {
            strategy.close("SHORT").here();
        }
    }
}
