// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from the script by Zippy111, https://www.tradingview.com/script/qN11DuKy/
// The original's notice: © Zippy111 and inspired by the youtuber Joel on Crypto
// Changed: rewritten by hand in Rust for vike.

//! A MACD histogram peak taken in the direction of the EMA trend, one trade at a time, size 1. Long when EMA(50) is at or over
//! EMA(200), the last cross of them (either way) was at most 350 bars ago, the MACD(12, 26, 9) histogram is under 0 and this
//! bar's histogram size is a ten-bar peak by the rule's own test (the strict highest of the last 10, or the lowest of the last
//! 10 as the bar before saw it); short on the mirror (histogram over 0 and a strict ten-bar high now or as the bar before saw
//! it). Each entry carries one exit from the bar after its fill, a target 0.5% from the average entry and a stop 0.4% the other
//! way. No costs; version 5 starts with 1,000,000.
//!
//! The library the rule imports (a pullback bar count) feeds only a filter that is off, so it is not ported. Its other
//! filters (RSI, pullback, trade pause, histogram average) are off, and its date range (2020 to 2099) holds on every bar.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Donchian, Ema, Macd};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::helpers::BarsSince;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const LONG_NAME: &str = "LONG open";
const SHORT_NAME: &str = "SHORT open";
const TREND_BARS: usize = 350;
const PEAK: usize = 10;
const TARGET: f64 = 0.5 * 0.01;
const STOP: f64 = 0.4 * 0.01;

/// Where the extremes of a series' last `PEAK` values lie, as this bar and the bar before left them: the reference's
/// `highestbars` / `lowestbars` (a tie goes to the older bar), `[high, low]` bars back.
struct Peaks {
    window: Donchian,
    now: Option<[usize; 2]>,
    before: Option<[usize; 2]>,
}

impl Peaks {
    fn new() -> Self {
        let window = Donchian::new(PEAK as f64).expect("a valid length").with_extreme_offsets(true);
        Peaks { window, now: None, before: None }
    }

    fn next(&mut self, x: f64) {
        self.window.next_hl(x, x);
        self.before = std::mem::replace(&mut self.now, self.window.extreme_offsets());
    }
}

/// Whether `side` (0 the high, 1 the low) of `offsets` is the bar itself: `highestbars(x, PEAK) == 0`.
fn on_bar_itself(offsets: Option<[usize; 2]>, side: usize) -> bool {
    offsets.is_some_and(|o| o[side] == 0)
}

struct S {
    fast: Ema,
    slow: Ema,
    macd: Macd,
    cross: Cross,
    since_up: BarsSince,
    since_down: BarsSince,
    hists: Peaks,
    abs_hists: Peaks,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: Ema::new(50.0).expect("a valid length"),
        slow: Ema::new(200.0).expect("a valid length"),
        macd: Macd::new(12.0, 26.0, 9.0).expect("valid lengths"),
        cross: Cross::new(),
        since_up: BarsSince::default(),
        since_down: BarsSince::default(),
        hists: Peaks::new(),
        abs_hists: Peaks::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let fast = self.fast.next_value(bar.close);
        let slow = self.slow.next_value(bar.close);
        let signal = self.cross.step(fast, slow);
        let since_up = self.since_up.step(matches!(signal, Signal::Long));
        let since_down = self.since_down.step(matches!(signal, Signal::Short));
        let [_, _, hist] = self.macd.next_value(bar.close);
        self.hists.next(hist);
        self.abs_hists.next(hist.abs());

        let highest = on_bar_itself(self.hists.now, 0) || on_bar_itself(self.hists.before, 0);
        let lowest =
            on_bar_itself(self.abs_hists.now, 0) || on_bar_itself(self.abs_hists.before, 1);
        let recent = |since: Option<usize>| since.is_some_and(|n| n <= TREND_BARS);
        let long = fast >= slow && recent(since_up) && hist < 0.0 && hist.abs() > 0.0 && lowest;
        let short = fast < slow && recent(since_down) && hist > 0.0 && highest;

        if strategy.position() == 0.0 {
            if long {
                strategy.entry(LONG_NAME, Long).size(1.0);
            }
            if short {
                strategy.entry(SHORT_NAME, Short).size(1.0);
            }
        }
        let held = strategy.position();
        if held != 0.0 {
            let avg = strategy.avg_cost();
            let (name, lots, take, cut) = if held > 0.0 {
                ("Long close", LONG_NAME, avg * (1.0 + TARGET), avg * (1.0 - STOP))
            } else {
                ("Short close", SHORT_NAME, avg * (1.0 - TARGET), avg * (1.0 + STOP))
            };
            strategy.exit(name).from(lots).take(take).stop(cut);
        }
    }
}
