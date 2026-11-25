// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "EmperorBTC's VWAP Strategy" by vaidab, https://www.tradingview.com/script/wmJ1ovZQ/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2021-06-10, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Long 1 when, on the bar before, the stochastic RSI's %K crossed up through its %D (on that bar or the one before it) while a
//! green bar's range held the day's VWAP (on that bar or the one before it), and the bar before also crossed up through one of
//! the day's Fibonacci pivot levels from below, its close and this one's both above their opens; short 1 on the mirror. One
//! position named `VWAP`, each side reversing the other; it closes at the next open once the close is 1% past its cost in its
//! favour. No costs; starts with 100,000.
//!
//! The pivot levels are the previous UTC day's: the rule asks for the daily bar before with lookahead, which on every bar of a
//! day is the day before it, its own last bar included: the last day that ended by this bar's open, never the day in progress.
//! The days are the symbol's daily input, its history before the window included (`vike_tactics::higher::HigherTimeframeSourceCalendar`), and the levels
//! vike's `PivotPoints` of the Fibonacci type (0.382, 0.618 and 1 of the range) of them. The VWAP is the day's running sum of `ohlc4 * volume` over its volume, from the window's first
//! bar (vike's `Vwap` on the close source, fed the mean price). The stochastic RSI is SMA(3) of the 14-bar stochastic of
//! RSI(14), and %D SMA(3) of that: vike's `Stochrsi` %K with the original's RSI (`Stochrsi::with_rs_form`), smoothed by two
//! `Sma`s that fold their window afresh.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{PivotPoints, Sma, Stochrsi, Vwap, VwapSource};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::higher::{self, HigherTimeframeSourceCalendar};
use vike_tactics::{HigherTimeframePeriod, Rule, RuleDefaults, Tactics};

const NAME: &str = "VWAP";
const TAKE: f64 = 0.01;

/// What a bar leaves for the rule of the bars after it.
#[derive(Clone, Copy, Default)]
struct Seen {
    rsi_up: bool,
    rsi_down: bool,
    vwap_up: bool,
    vwap_down: bool,
    pivot_up: bool,
    pivot_down: bool,
    green: bool,
    red: bool,
}

struct S {
    days: HigherTimeframeSourceCalendar,
    pivots: PivotPoints,
    /// `(day's start, its levels [p, r1, r2, r3, s1, s2, s3])` of the last day closed, and of the day before it.
    last: (i64, [f64; 7]),
    before: (i64, [f64; 7]),
    vwap: Vwap,
    stoch: Stochrsi,
    k: Sma,
    d: Sma,
    kd: (f64, f64),
    /// The last three bars, newest first.
    seen: [Seen; 3],
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        days: HigherTimeframeSourceCalendar::new(HigherTimeframePeriod::Day),
        pivots: PivotPoints::default().with_fibonacci(true),
        last: (i64::MIN, [f64::NAN; 7]),
        before: (i64::MIN, [f64::NAN; 7]),
        vwap: Vwap::default().with_source(VwapSource::Close),
        stoch: Stochrsi::new(14.0, 14.0, 3.0).expect("valid lengths").with_rs_form(true),
        k: Sma::new(3.0).expect("a valid length").with_fresh_sum(true),
        d: Sma::new(3.0).expect("a valid length").with_fresh_sum(true),
        kd: (f64::NAN, f64::NAN),
        seen: [Seen::default(); 3],
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn inputs(&self) -> Vec<String> {
        vec![higher::DAY.to_string()]
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        for day in self.days.next(bar, strategy.input(higher::DAY)) {
            self.pivots.next_hlc(day.high, day.low, day.close);
            // `PivotPoints` hands out the levels of the bar fed before: those of this day come with the next one fed.
            let levels = self.pivots.clone().next_hlc(f64::NAN, f64::NAN, f64::NAN);
            self.before = std::mem::replace(&mut self.last, (day.timestamp, levels));
        }
        // On a day's last bar that day has just closed: the day before is the one before it.
        let (_, levels) = if HigherTimeframePeriod::Day.end(self.last.0) <= bar.timestamp {
            self.last
        } else {
            self.before
        };
        // The pivot and the three levels under it (NaN ones, before a day has come, cross nothing).
        let [p, _, _, _, s1, s2, s3] = levels;
        let supports = [s3, s2, s1, p];
        let pivot_up = supports.iter().any(|&s| bar.low < s && bar.close > s);
        let pivot_down = supports.iter().any(|&s| bar.high > s && bar.close < s);

        let ohlc4 = (bar.open + bar.high + bar.low + bar.close) / 4.0;
        let vwap = self.vwap.next_ts_hlcv(bar.timestamp, bar.high, bar.low, ohlc4, bar.volume);
        let (green, red) = (bar.close > bar.open, bar.close < bar.open);

        let [stoch, _] = self.stoch.next_value(bar.close);
        let k = self.k.next_value(stoch);
        let d = self.d.next_value(k);
        let (k1, d1) = self.kd;
        self.kd = (k, d);

        let now = Seen {
            rsi_up: k1 < d1 && k > d,
            rsi_down: k1 > d1 && k < d,
            vwap_up: bar.low < vwap && vwap < bar.high && green,
            vwap_down: bar.high > vwap && vwap > bar.low && red,
            pivot_up,
            pivot_down,
            green,
            red,
        };
        let [one, two, _] = self.seen;
        self.seen = [now, one, two];

        let long = (one.rsi_up || two.rsi_up)
            && (one.vwap_up || two.vwap_up)
            && one.pivot_up
            && one.green
            && green;
        let short = (one.rsi_down || two.rsi_down)
            && (one.vwap_down || two.vwap_down)
            && one.pivot_down
            && one.red
            && red;
        let held = strategy.position();
        let cost = strategy.avg_cost();
        let take = (held > 0.0 && bar.close > cost * (1.0 + TAKE))
            || (held < 0.0 && bar.close < cost * (1.0 - TAKE));
        if long {
            strategy.entry(NAME, Long).size(1.0);
        }
        if short {
            strategy.entry(NAME, Short).size(1.0);
        }
        if take {
            strategy.close(NAME);
        }
    }
}
