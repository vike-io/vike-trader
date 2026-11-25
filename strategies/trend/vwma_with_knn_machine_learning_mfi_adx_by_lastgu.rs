// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "VWMA with kNN Machine Learning: MFI/ADX by lastguru" by lastguru, https://www.tradingview.com/script/73oLZ8IM/
// The original's notice: © lastguru
// Changed: rewritten by hand in Rust for vike.

//! Volume-weighted averages of the open over 13 and 19 bars, filtered by nearest neighbours. On each cross of the two the rule
//! first files the previous cross: its ADX and money flow, and whether this open is at or above that cross's open. Then it
//! takes the 45 filed crosses nearest to this cross's ADX and money flow (every one at the 45th distance or nearer) and sums
//! their outcomes, +1 up and -1 down. Long all of the account's worth when the fast crosses up through the slow and that sum
//! is 2 or more, short all of it when it crosses down and the sum is -2 or less, each reversing the other. No costs; starts with
//! 100,000.
//!
//! The ADX (13-bar directional lines, 6-bar smoothing) runs on the rule's own Wilder average, which starts from 0 rather than
//! from a mean (vike's `Smma::with_zero_seed`), with the true range NaN on the first bar. The money flow sums volume times the mean price (open, high, low
//! and close over four) over 13 bars on the bars it rose and on the bars it fell, the first bar counting on both sides. The
//! store starts with one neighbour at 0 and 0 whose outcome is 0, and the first cross files another at 0 and 0.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Smma, Vwma};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::rolling::RollingSum;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const FAST: f64 = 13.0;
const SLOW: f64 = 19.0;
const FILTER: f64 = 13.0;
const SMOOTH: f64 = 6.0;
const NEIGHBOURS: usize = 45;
const MIN_SUM: f64 = 2.0;

/// The rule's Wilder average: `((length - 1) * previous + x) / length`, a NaN previous read as 0 (vike's in its own
/// rounding).
fn zero_rma(length: f64) -> Smma {
    Smma::new(length).expect("a valid length").with_zero_seed(true)
}

struct S {
    fast: Vwma,
    slow: Vwma,
    cross: Cross,
    /// The previous bar's `(high, low, close, mean price)`.
    last: (f64, f64, f64, f64),
    range: Smma,
    plus_rma: Smma,
    minus_rma: Smma,
    adx_rma: Smma,
    /// The directional lines, each holding its last number over a NaN.
    plus: f64,
    minus: f64,
    flow_up: RollingSum,
    flow_down: RollingSum,
    /// The filed crosses: `(adx, money flow, outcome)`.
    filed: Vec<(f64, f64, f64)>,
    /// The cross waiting for its outcome: `(adx, money flow, open)`.
    waiting: (f64, f64, f64),
    /// `vote`'s buffers, kept so that a cross allocates nothing: every filed cross's distance in filing order, and the
    /// copy the nearest are selected in.
    distance: Vec<f64>,
    nearest: Vec<f64>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: Vwma::new(FAST).expect("a valid length"),
        slow: Vwma::new(SLOW).expect("a valid length"),
        cross: Cross::new(),
        last: (f64::NAN, f64::NAN, f64::NAN, f64::NAN),
        range: zero_rma(FILTER),
        plus_rma: zero_rma(FILTER),
        minus_rma: zero_rma(FILTER),
        adx_rma: zero_rma(SMOOTH),
        plus: f64::NAN,
        minus: f64::NAN,
        flow_up: RollingSum::new(FILTER as usize),
        flow_down: RollingSum::new(FILTER as usize),
        filed: vec![(0.0, 0.0, 0.0)],
        waiting: (0.0, 0.0, 0.0),
        distance: Vec::new(),
        nearest: Vec::new(),
    })
}

impl S {
    // LEFT: no `Adx` option computes this one (sums from 0 on the second bar, DI held, the ADX a Wilder average from 0).
    fn adx(&mut self, bar: &Bar) -> f64 {
        let (high, low, close, _) = self.last;
        let up = bar.high - high;
        let down = -(bar.low - low);
        let plus_dm = if up.is_nan() {
            f64::NAN
        } else if up > down && up > 0.0 {
            up
        } else {
            0.0
        };
        let minus_dm = if down.is_nan() {
            f64::NAN
        } else if down > up && down > 0.0 {
            down
        } else {
            0.0
        };
        let true_range = if close.is_nan() {
            f64::NAN
        } else {
            (bar.high - bar.low).max((bar.high - close).abs()).max((bar.low - close).abs())
        };
        let range = self.range.next_value(true_range);
        let plus = 100.0 * self.plus_rma.next_value(plus_dm) / range;
        if !plus.is_nan() {
            self.plus = plus;
        }
        let minus = 100.0 * self.minus_rma.next_value(minus_dm) / range;
        if !minus.is_nan() {
            self.minus = minus;
        }
        let sum = self.plus + self.minus;
        let spread = (self.plus - self.minus).abs() / if sum == 0.0 { 1.0 } else { sum };
        100.0 * self.adx_rma.next_value(spread)
    }

    // Not vike's `Mfi`: that takes the typical price of high, low and close (no ohlc4) and counts its first bar on neither side.
    fn money_flow(&mut self, bar: &Bar) -> f64 {
        let price = (bar.open + bar.high + bar.low + bar.close) / 4.0;
        let change = price - self.last.3;
        // A NaN change is neither at most 0 nor at least 0, so the first bar's flow counts on both sides.
        let up = self.flow_up.next(bar.volume * if change <= 0.0 { 0.0 } else { price });
        let down = self.flow_down.next(bar.volume * if change >= 0.0 { 0.0 } else { price });
        100.0 - 100.0 / (1.0 + up / down)
    }

    /// Files the waiting cross with this open's outcome, and makes this cross the waiting one.
    fn file(&mut self, adx: f64, flow: f64, open: f64) {
        let (p1, p2, before) = self.waiting;
        self.filed.push((p1, p2, if open >= before { 1.0 } else { -1.0 }));
        self.waiting = (adx, flow, open);
    }

    /// The sum of the outcomes of every filed cross at or nearer than the `NEIGHBOURS`-th nearest distance.
    fn vote(&mut self, adx: f64, flow: f64) -> f64 {
        self.distance.clear();
        self.distance.extend(self.filed.iter().map(|&(p1, p2, _)| {
            let (da, df) = (adx - p1, flow - p2);
            (da * da + df * df).sqrt()
        }));
        self.nearest.clone_from(&self.distance);
        let count = NEIGHBOURS.min(self.nearest.len());
        // Only the largest of the nearest is read, and a `total_cmp` tie is the same number, so selecting them unordered
        // (linear) reads what sorting every distance did.
        self.nearest.select_nth_unstable_by(count - 1, f64::total_cmp);
        let farthest = self.nearest[..count].iter().copied().fold(f64::NEG_INFINITY, f64::max);
        self.distance
            .iter()
            .zip(&self.filed)
            .filter(|(d, _)| **d <= farthest)
            .map(|(_, f)| f.2)
            .sum()
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let src = bar.open;
        let fast = self.fast.next_cv(src, bar.volume);
        let slow = self.slow.next_cv(src, bar.volume);
        let adx = self.adx(bar);
        let flow = self.money_flow(bar);
        self.last =
            (bar.high, bar.low, bar.close, (bar.open + bar.high + bar.low + bar.close) / 4.0);
        let cross = self.cross.step(fast, slow);
        if matches!(cross, Signal::Hold) {
            return;
        }
        self.file(adx, flow, src);
        let vote = self.vote(adx, flow);
        if matches!(cross, Signal::Long) && vote >= MIN_SUM {
            strategy.entry("long", Long).size(share_of_worth(&sizing, 100.0, bar.close));
        }
        if matches!(cross, Signal::Short) && vote <= -MIN_SUM {
            strategy.entry("short", Short).size(share_of_worth(&sizing, 100.0, bar.close));
        }
    }
}
