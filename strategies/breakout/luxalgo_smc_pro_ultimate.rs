// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "LuxAlgo SMC Pro Ultimate" by HAC19, https://www.tradingview.com/script/mEHJ2lCm/
// Changed: rewritten by hand in Rust for vike.

//! Swing points: a high 9 bars back that is the highest of the 19 bars around it is an internal swing high, one 50 bars back
//! the highest of 101 a swing high; lows the mirror, each kept until the next. Flat, long 10% of the account's worth when the
//! close crosses up through either swing high, below the middle of the last 100 bars' range, with a fair value gap up (the low
//! above the high two bars before, and the close before above it too) on this bar or the one before; short on the mirror.
//! The stop starts 3 ATR(9) beyond the signal bar's far end and the target 1.5 times that risk from its close. From the close
//! of the first bar held: once a high reaches the target the stop moves to the average cost, and it then trails 3 ATR(9)
//! beyond each bar's far end, only ever tightening, for the whole position. No costs; starts with 10,000.
//!
//! The script also rests half the position as a limit at the target once a high reaches it, an exit issued after the stop's,
//! which already holds the whole position. This rule issues that half target as the script does; under its row's
//! `bound_exits` (`vike_tactics::RuleRun::bound_exits`) the exits of an entry share its quantity in the order they were issued,
//! so the half target finds nothing left and gets no order beside the stop.
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
use vike_tactics::helpers::{back, push};
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::{floor_share, share_of_worth};
use vike_tactics::{Rule, RuleDefaults, Tactics};

use vike_indicators::indicators::Donchian;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;

const UP: &str = "long";
const DOWN: &str = "short";
const INTERNAL: usize = 9;
const SWING: usize = 50;
const ATRS: f64 = 3.0;
const REWARD: f64 = 1.5;
/// The share of the entry the half target exits.
const HALF: f64 = 0.5;

/// A swing level of one span: the price `span` bars back when it is the extreme of the `2 * span + 1` bars around it.
// Not vike's `ConfirmedPivot`: a tie on either side counts here, where vike's newer bars must lose.
struct Swing {
    span: usize,
    window: Donchian,
    highs: VecDeque<f64>,
    lows: VecDeque<f64>,
    high: f64,
    low: f64,
}

impl Swing {
    fn new(span: usize) -> Self {
        Swing {
            span,
            window: Donchian::new((2 * span + 1) as f64).expect("a valid length"),
            highs: VecDeque::with_capacity(span + 2),
            lows: VecDeque::with_capacity(span + 2),
            high: f64::NAN,
            low: f64::NAN,
        }
    }

    fn step(&mut self, bar: &Bar) {
        let [highest, _, lowest] = self.window.next(bar);
        self.highs.push_front(bar.high);
        self.lows.push_front(bar.low);
        self.highs.truncate(self.span + 1);
        self.lows.truncate(self.span + 1);
        if self.highs.get(self.span) == Some(&highest) {
            self.high = highest;
        }
        if self.lows.get(self.span) == Some(&lowest) {
            self.low = lowest;
        }
    }
}

/// The rule's exit of half the entry at `target`, issued on the bar a high (a low) first reaches it.
fn half_target(strategy: &mut Tactics<'_>, lots: &str, name: &str, target: f64) {
    let sizing = strategy.sizing();
    let entry = strategy.bindings().iter().find(|b| b.name == lots).map_or(0.0, |b| b.size);
    let size = floor_share(entry * HALF, entry, sizing.qty_step());
    strategy.exit(name).from(lots).size(size).take(target);
}

struct S {
    atr: IndicatorLine,
    range: Donchian,
    internal: Swing,
    swing: Swing,
    crosses: [Cross; 4],
    /// High, low and close, newest first, three of each.
    highs: VecDeque<f64>,
    lows: VecDeque<f64>,
    closes: VecDeque<f64>,
    gap_up_before: bool,
    gap_down_before: bool,
    stop: f64,
    target: f64,
    target_hit: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        atr: IndicatorLine::new("atr", &[9.0]),
        range: Donchian::new(100.0).expect("a valid length"),
        internal: Swing::new(INTERNAL),
        swing: Swing::new(SWING),
        crosses: std::array::from_fn(|_| Cross::new()),
        highs: VecDeque::with_capacity(4),
        lows: VecDeque::with_capacity(4),
        closes: VecDeque::with_capacity(4),
        gap_up_before: false,
        gap_down_before: false,
        stop: f64::NAN,
        target: f64::NAN,
        target_hit: false,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let atr = self.atr.next(bar)[0];
        let [top, _, bottom] = self.range.next(bar);
        let middle = (top + bottom) / 2.0;
        self.internal.step(bar);
        self.swing.step(bar);
        push(&mut self.highs, bar.high, 3);
        push(&mut self.lows, bar.low, 3);
        push(&mut self.closes, bar.close, 3);
        let (h, l, c) = (bar.high, bar.low, bar.close);
        let gap_up = l > back(&self.highs, 2) && back(&self.closes, 1) > back(&self.highs, 2);
        let gap_down = h < back(&self.lows, 2) && back(&self.closes, 1) < back(&self.lows, 2);
        let levels = [self.internal.high, self.internal.low, self.swing.high, self.swing.low];
        let [ih, il, sh, sl] = std::array::from_fn(|k| self.crosses[k].step(c, levels[k]));
        let broke_up = matches!(ih, Signal::Long) || matches!(sh, Signal::Long);
        let broke_down = matches!(il, Signal::Short) || matches!(sl, Signal::Short);
        let buy = broke_up && c < middle && (self.gap_up_before || gap_up);
        let sell = broke_down && c > middle && (self.gap_down_before || gap_down);
        (self.gap_up_before, self.gap_down_before) = (gap_up, gap_down);

        let held = strategy.position();
        if buy && held == 0.0 {
            self.stop = l - atr * ATRS;
            self.target = c + (c - self.stop) * REWARD;
            self.target_hit = false;
            strategy.entry(UP, Long).size(share_of_worth(&sizing, 10.0, c));
        }
        if sell && held == 0.0 {
            self.stop = h + atr * ATRS;
            self.target = c - (self.stop - c) * REWARD;
            self.target_hit = false;
            strategy.entry(DOWN, Short).size(share_of_worth(&sizing, 10.0, c));
        }
        if held > 0.0 {
            if !self.target_hit && h >= self.target {
                self.target_hit = true;
                half_target(strategy, UP, "TP1_L", self.target);
                self.stop = strategy.avg_cost();
            }
            let trail = l - atr * ATRS;
            if trail > self.stop {
                self.stop = trail;
            }
            strategy.exit("exit").from(UP).stop(self.stop);
        }
        if held < 0.0 {
            if !self.target_hit && l <= self.target {
                self.target_hit = true;
                half_target(strategy, DOWN, "TP1_S", self.target);
                self.stop = strategy.avg_cost();
            }
            let trail = h + atr * ATRS;
            if trail < self.stop {
                self.stop = trail;
            }
            strategy.exit("exit").from(DOWN).stop(self.stop);
        }
    }
}
