// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "TradeIQ - Crazy Scalping Trading Strategy [Kaspricci]" by Best-Algo-Trader, https://www.tradingview.com/script/R7OSA2ar/
// The original's notice: © Kaspricci
// Changed: rewritten by hand in Rust for vike.

//! Divergences of a momentum oscillator, `mom(sma(sma(close, 7), 7), 7)`, read where its one-bar change turns: bearish when it
//! turns down above 0 at a lower value than at the last such turn while the 7-bar highest close is higher, bullish the mirror.
//! A band follows a weighted filter of the close over 50 bars, two 50-bar mean deviations wide; a high crossing the upper band
//! sets the state up, a low crossing the lower one sets it down.
//!
//! When flat, on the first bar of three in which the filtered close moves further and further from the mean of its two previous
//! values: long 1 if a bullish divergence in the up state came more recently than both the last down state and the last
//! bearish divergence, short 1 on the mirror. A long's stop lies as far under its entry fill as the signal close was over the
//! 10-bar lowest low, a short's the mirror. A long closes at the next open on a bearish divergence or in the down state, a short
//! the mirror. No costs; starts with 1,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use vike_tactics::helpers::{BarsSince, fewer};
use vike_tactics::rolling::CrossEither;
use vike_tactics::{Rule, RuleDefaults, Tactics};

use vike_indicators::indicators::{Donchian, Mom, Sma};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;

const UP: &str = "long";
const DOWN: &str = "short";
const TMO_LENGTH: f64 = 7.0;
const AMA_LENGTH: usize = 50;
const AMA_FACTOR: f64 = 2.0;
const SWING_LENGTH: f64 = 10.0;
#[expect(clippy::approx_constant, reason = "the rule writes 3.14159, and the weights are its")]
const RULE_PI: f64 = 3.14159;

/// The rule's weighted filter: the last 50 values, the newest first, each times `sin(2 * 3.14159 * x) * (1 - sqrt(x))` with `x`
/// running 0 to 1, over the weights' sum. NaN until 50 have come.
struct Filter {
    weights: Vec<f64>,
    total: f64,
    values: VecDeque<f64>,
}

impl Filter {
    fn new() -> Self {
        let weights: Vec<f64> = (0..AMA_LENGTH)
            .map(|i| {
                let x = i as f64 / (AMA_LENGTH - 1) as f64;
                libm::sin(2.0 * RULE_PI * libm::pow(x, 1.0)) * (1.0 - libm::pow(x, 0.5))
            })
            .collect();
        let total = weights.iter().fold(0.0, |s, w| s + w);
        Filter { weights, total, values: VecDeque::with_capacity(AMA_LENGTH + 1) }
    }

    fn next(&mut self, x: f64) -> f64 {
        self.values.push_front(x);
        self.values.truncate(AMA_LENGTH);
        if self.values.len() < AMA_LENGTH {
            return f64::NAN;
        }
        let sum = self.values.iter().zip(&self.weights).fold(0.0, |s, (v, w)| s + v * w);
        sum / self.total
    }
}

/// An SMA fed only from its source's first number on.
fn sma_of(sma: &mut Sma, x: f64) -> f64 {
    if x.is_nan() { f64::NAN } else { sma.next_value(x) }
}

struct S {
    inner: Sma,
    outer: Sma,
    mom: Mom,
    closes: Donchian,
    previous_osc: f64,
    previous_change: f64,
    /// `(osc, highest close)` at the last downturn, `(osc, lowest close)` at the last upturn.
    last_top: (f64, f64),
    last_bottom: (f64, f64),
    close_filter: Filter,
    deviation: Sma,
    cross_high: CrossEither,
    cross_low: CrossEither,
    /// Up, down, or neither before the first cross: the rule's state reads its own previous bar, which the first bar lacks.
    state: Option<bool>,
    filtered: VecDeque<f64>,
    bodies: VecDeque<f64>,
    increasing_before: bool,
    since_bull_up: BarsSince,
    since_bear_down: BarsSince,
    since_down: BarsSince,
    since_up: BarsSince,
    since_bear: BarsSince,
    since_bull: BarsSince,
    swing: Donchian,
    stop_distance: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        inner: Sma::new(TMO_LENGTH).expect("a valid length"),
        outer: Sma::new(TMO_LENGTH).expect("a valid length"),
        mom: Mom::new(TMO_LENGTH).expect("a valid length"),
        closes: Donchian::new(TMO_LENGTH).expect("a valid length"),
        previous_osc: f64::NAN,
        previous_change: f64::NAN,
        last_top: (f64::NAN, f64::NAN),
        last_bottom: (f64::NAN, f64::NAN),
        close_filter: Filter::new(),
        deviation: Sma::new(AMA_LENGTH as f64).expect("a valid length"),
        cross_high: CrossEither::default(),
        cross_low: CrossEither::default(),
        state: None,
        filtered: VecDeque::with_capacity(4),
        bodies: VecDeque::with_capacity(4),
        increasing_before: false,
        since_bull_up: BarsSince::default(),
        since_bear_down: BarsSince::default(),
        since_down: BarsSince::default(),
        since_up: BarsSince::default(),
        since_bear: BarsSince::default(),
        since_bull: BarsSince::default(),
        swing: Donchian::new(SWING_LENGTH).expect("a valid length"),
        stop_distance: f64::NAN,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let smoothed = sma_of(&mut self.outer, sma_of(&mut self.inner, bar.close));
        let osc = if smoothed.is_nan() { f64::NAN } else { self.mom.next_value(smoothed) };
        let [highest_close, _, lowest_close] = self.closes.next_hl(bar.close, bar.close);
        let change = osc - self.previous_osc;
        self.previous_osc = osc;
        let turned_down = change < 0.0 && self.previous_change >= 0.0;
        let turned_up = change > 0.0 && self.previous_change <= 0.0;
        self.previous_change = change;
        let mut bear = false;
        if turned_down {
            bear = osc > 0.0 && osc < self.last_top.0 && highest_close > self.last_top.1;
            self.last_top = (osc, highest_close);
        }
        let mut bull = false;
        if turned_up {
            bull = osc < 0.0 && osc > self.last_bottom.0 && lowest_close < self.last_bottom.1;
            self.last_bottom = (osc, lowest_close);
        }

        let filtered = self.close_filter.next(bar.close);
        let deviation = sma_of(&mut self.deviation, (bar.close - filtered).abs()) * AMA_FACTOR;
        let crossed_high = self.cross_high.step(bar.high, filtered + deviation);
        let crossed_low = self.cross_low.step(bar.low, filtered - deviation);
        if crossed_high {
            self.state = Some(true);
        } else if crossed_low {
            self.state = Some(false);
        }
        let (up, down) = (self.state == Some(true), self.state == Some(false));
        self.filtered.push_front(filtered);
        self.filtered.truncate(3);
        let f = |i: usize| self.filtered.get(i).copied().unwrap_or(f64::NAN);
        self.bodies.push_front(((f(1) + f(2)) / 2.0 - f(0)).abs());
        self.bodies.truncate(3);
        let b = |i: usize| self.bodies.get(i).copied().unwrap_or(f64::NAN);
        let increasing = b(2) < b(1) && b(1) < b(0);
        let fresh = increasing && !self.increasing_before;
        self.increasing_before = increasing;

        let since_bull_up = self.since_bull_up.step(up && bull);
        let since_bear_down = self.since_bear_down.step(down && bear);
        let since_down = self.since_down.step(down);
        let since_up = self.since_up.step(up);
        let since_bear = self.since_bear.step(bear);
        let since_bull = self.since_bull.step(bull);
        let [swing_high, _, swing_low] = self.swing.next(bar);

        let held = strategy.position();
        let long_entry = held == 0.0
            && fresh
            && fewer(since_bull_up, since_down)
            && fewer(since_bull_up, since_bear);
        let short_entry = held == 0.0
            && fresh
            && fewer(since_bear_down, since_up)
            && fewer(since_bear_down, since_bull);
        let long_exit = held > 0.0 && (bear || down);
        let short_exit = held < 0.0 && (bull || up);
        if long_entry {
            self.stop_distance = bar.close - swing_low;
            strategy.entry(UP, Long).size(1.0);
        }
        if long_exit {
            strategy.close(UP);
        }
        if short_entry {
            self.stop_distance = swing_high - bar.close;
            strategy.entry(DOWN, Short).size(1.0);
        }
        if short_exit {
            strategy.close(DOWN);
        }
    }

    /// The stop works from the entry's fill, so it is set once the entry has filled.
    // The exits' levels are measured from the entry's fill, so they are issued here, after it; under the simulator's price
    // order (`[engine.in_bar] price_order`) they work from the fill's own level on.
    fn after_fill(&mut self, strategy: &mut Tactics<'_>, _bar: &Bar) {
        let held = strategy.position();
        let cost = strategy.avg_cost();
        if held > 0.0 {
            strategy.exit("Stop Loss Long").from(UP).stop(cost - self.stop_distance);
        } else if held < 0.0 {
            strategy.exit("Stop Loss Short").from(DOWN).stop(cost + self.stop_distance);
        }
    }
}
