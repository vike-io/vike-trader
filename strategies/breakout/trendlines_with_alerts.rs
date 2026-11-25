// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "TrendLines with Alerts" by puneetghai, https://www.tradingview.com/script/vOyqoCwS/
// The original's notice: © pune3tghai
// Changed: rewritten by hand in Rust for vike.

//! Two trend lines. A high 9 bars back that is the highest of the last 20 bars, and was also the highest of the 20 bars as they
//! stood 9 bars ago, is a new upper pivot; the line runs from the pivot before it through this one, at the slope between them,
//! and an extension projects it 18 bars further from where it stood 9 bars ago. The lows make the lower line the same way. Long
//! 1 when a bar dips to the lower line (or its extension) and opens, peaks and closes above it, or its low crosses up through
//! the upper extension; short 1 on the mirror, each reversing the other. While held, a target and a stop, moved every bar: the
//! rule gives them as tick counts equal to a price (the long's target the close of the buy signal before the last one, plus
//! 0.2%; its stop the last buy signal's close less 0.2%), so they lie that many ticks from the entry. No costs; starts with
//! 100,000. **Does not reproduce its reference** (1592 trades, net -24030.519531 on BTCUSDT 30m, 2025-01-01..2026-09-22):
//! this file gives 1592 / -24029.380000, the net 1.14 over with no cause known (level roundings, raw levels, bar-walk ties
//! and frozen levels each give 0.55 to 5.21 off or worse); the owner accepted those numbers on 2026-11-15.
//!
//! A bar with both signals takes the long and then the short.
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
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const UP: &str = "long";
const DOWN: &str = "short";
const CHECK: usize = 9;
const SHARE: f64 = 0.2 / 100.0;

/// One trend line's state, newest values pushed onto each series every bar.
struct Trend {
    /// The pivot price, the previous pivot, the slope.
    pivot: f64,
    before: f64,
    slope: f64,
    /// Bars since the pivot equalled the price `CHECK` bars back, NaN before it ever did.
    since: Vec<f64>,
    slopes: Vec<f64>,
    line: Vec<f64>,
}

impl Trend {
    fn new() -> Self {
        Trend {
            pivot: f64::NAN,
            before: f64::NAN,
            slope: f64::NAN,
            since: Vec::new(),
            slopes: Vec::new(),
            line: Vec::new(),
        }
    }

    /// `prices` holds every price so far, this bar's last; `found` is whether this bar sets a new pivot.
    fn step(&mut self, prices: &[f64], found: bool) {
        let i = prices.len() - 1;
        let past = |k: f64| -> f64 {
            if k.is_nan() || k as usize > i { f64::NAN } else { prices[i - k as usize] }
        };
        if found {
            self.pivot = past(CHECK as f64);
        }
        let last_since = self.since.last().copied().unwrap_or(f64::NAN);
        let since = if self.pivot == past(CHECK as f64) { 0.0 } else { last_since + 1.0 };
        self.since.push(since);
        if since == 0.0 && i > CHECK {
            self.before = past((last_since + 1.0 + CHECK as f64).abs());
        }
        if since == 0.0 {
            self.slope = (self.pivot - self.before) / last_since;
        }
        self.slopes.push(self.slope);
        self.line.push(self.pivot + since * self.slope);
    }

    fn now(&self) -> f64 {
        *self.line.last().expect("stepped")
    }

    /// The line as it stood `CHECK` bars ago carried `2 * CHECK` bars on, from bar `CHECK + 1` on.
    fn extension(&self) -> f64 {
        let i = self.line.len() - 1;
        if i > CHECK {
            self.line[i - CHECK] + self.slopes[i - CHECK] * CHECK as f64 * 2.0
        } else {
            f64::NAN
        }
    }
}

struct S {
    range: Donchian,
    highs: Vec<f64>,
    lows: Vec<f64>,
    upper_ends: Vec<f64>,
    lower_ends: Vec<f64>,
    upper: Trend,
    lower: Trend,
    /// The extensions on the bar before, for the crosses.
    up_extension_before: f64,
    down_extension_before: f64,
    high_before: f64,
    low_before: f64,
    /// The closes of the last two buy and sell signals, newest first.
    buys: [f64; 2],
    sells: [f64; 2],
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        range: Donchian::new(20.0).expect("a valid length"),
        highs: Vec::new(),
        lows: Vec::new(),
        upper_ends: Vec::new(),
        lower_ends: Vec::new(),
        upper: Trend::new(),
        lower: Trend::new(),
        up_extension_before: f64::NAN,
        down_extension_before: f64::NAN,
        high_before: f64::NAN,
        low_before: f64::NAN,
        buys: [f64::NAN; 2],
        sells: [f64::NAN; 2],
    })
}

fn back(series: &[f64], k: usize) -> f64 {
    if k < series.len() { series[series.len() - 1 - k] } else { f64::NAN }
}

impl S {
    /// The target and stop of whatever is held, `ticks` from its average cost.
    fn protect(&self, strategy: &mut Tactics<'_>) {
        let held = strategy.position();
        let cost = strategy.avg_cost();
        let tick = strategy.tick().unwrap_or(0.0);
        let distance = |ticks: f64| ticks * tick;
        if held > 0.0 {
            let take = distance(self.buys[1] + SHARE * self.buys[1]);
            let stop = distance(self.buys[0] - SHARE * self.buys[0]);
            let exit = strategy.exit("xl").from(UP);
            if !take.is_nan() {
                exit.take(cost + take);
            }
            if !stop.is_nan() {
                exit.stop(cost - stop);
            }
        } else if held < 0.0 {
            let take = distance(self.sells[1] - SHARE * self.sells[1]);
            let stop = distance(self.sells[0] + SHARE * self.sells[0]);
            let exit = strategy.exit("xs").from(DOWN);
            if !take.is_nan() {
                exit.take(cost - take);
            }
            if !stop.is_nan() {
                exit.stop(cost + stop);
            }
        }
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let [highest, _, lowest] = self.range.next(bar);
        self.highs.push(bar.high);
        self.lows.push(bar.low);
        self.upper_ends.push(highest);
        self.lower_ends.push(lowest);
        let i = self.highs.len() - 1;
        let up_found = highest == back(&self.highs, CHECK)
            && highest == back(&self.upper_ends, CHECK)
            && i > CHECK;
        let down_found = lowest == back(&self.lows, CHECK)
            && lowest == back(&self.lower_ends, CHECK)
            && i > CHECK;
        self.upper.step(&self.highs, up_found);
        self.lower.step(&self.lows, down_found);
        let (upper, lower) = (self.upper.now(), self.lower.now());
        let (up_extension, down_extension) = (self.upper.extension(), self.lower.extension());
        let (o, h, l, c) = (bar.open, bar.high, bar.low, bar.close);
        let down_cross = h < down_extension && self.high_before >= self.down_extension_before;
        let up_cross = l > up_extension && self.low_before <= self.up_extension_before;
        let dips = |line: f64| l <= line && o > line && h > line && c > line;
        let peaks = |line: f64| h >= line && o < line && l < line && c < line;
        let buy = dips(lower) || dips(down_extension) || up_cross;
        let sell = peaks(upper) || peaks(up_extension) || down_cross;
        (self.up_extension_before, self.down_extension_before) = (up_extension, down_extension);
        (self.high_before, self.low_before) = (h, l);
        if buy {
            self.buys = [c, self.buys[0]];
            strategy.entry(UP, Long).size(1.0);
        }
        if sell {
            self.sells = [c, self.sells[0]];
            strategy.entry(DOWN, Short).size(1.0);
        }
        self.protect(strategy);
    }

    fn after_fill(&mut self, strategy: &mut Tactics<'_>, _bar: &Bar) {
        self.protect(strategy);
    }
}
