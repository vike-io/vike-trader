// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Simple Harmonic Pattern Strategy" by blitz_locked, https://www.tradingview.com/script/edChhkxo/
// Changed: rewritten by hand in Rust for vike.

//! A zigzag of pivots with 4 bars on each side: a pivot of the same kind as the last one replaces it when it lies further out,
//! and otherwise each pivot is a new point. On every bar that confirms a pivot, the last five points X, A, B, C, D are scored
//! against four harmonic shapes (Gartley, Bat, Butterfly, Crab): each checks AB/XA, BC/AB, CD/BC and XD/XA against its range
//! widened by 0.1 either side, and three checks of one shape are a match. A match ending on a low goes long 10% of the
//! account's worth, one ending on a high short, each reversing the other, unless an entry was signalled under 5 bars ago.
//! With it, a stop a quarter of ATR(14) beyond X and a target twice that risk from D, both resting from the fill; a match on
//! the side already held moves them. 0.05% commission; starts with 10,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::ConfirmedPivot;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const UP: &str = "long";
const DOWN: &str = "short";
const KEPT: usize = 20;
const TOLERANCE: f64 = 0.10;
const ENOUGH: usize = 3;
const COOLDOWN: i64 = 5;
const STOP_ATRS: f64 = 0.25;
const REWARD: f64 = 2.0;

/// Each shape's four ranges, for AB/XA, BC/AB, CD/BC and XD/XA, before the tolerance widens them.
const SHAPES: [[(f64, f64); 4]; 4] = [
    [(0.618, 0.618), (0.382, 0.886), (1.13, 1.618), (0.786, 0.786)],
    [(0.382, 0.5), (0.382, 0.886), (1.618, 2.618), (0.886, 0.886)],
    [(0.786, 0.786), (0.382, 0.886), (1.618, 2.618), (1.27, 1.618)],
    [(0.382, 0.618), (0.382, 0.886), (2.24, 3.618), (1.618, 1.618)],
];

/// A zigzag point: its price and `1` for a high, `-1` for a low.
type Point = (f64, i32);

struct S {
    pivots: ConfirmedPivot,
    atr: IndicatorLine,
    points: Vec<Point>,
    bar: i64,
    last_entry: Option<i64>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        pivots: ConfirmedPivot::new(4.0, 4.0).expect("valid spans"),
        atr: IndicatorLine::new("atr", &[14.0]),
        points: Vec::with_capacity(KEPT + 1),
        bar: 0,
        last_entry: None,
    })
}

impl S {
    fn push(&mut self, price: f64, dir: i32) {
        match self.points.last_mut() {
            Some(last) if last.1 == dir => {
                if (dir == 1 && price > last.0) || (dir == -1 && price < last.0) {
                    last.0 = price;
                }
            }
            _ => self.points.push((price, dir)),
        }
        if self.points.len() > KEPT {
            self.points.remove(0);
        }
    }
}

fn score(shape: &[(f64, f64); 4], ratios: [f64; 4]) -> usize {
    shape
        .iter()
        .zip(ratios)
        .filter(|&(&(lo, hi), r)| r >= lo - TOLERANCE && r <= hi + TOLERANCE)
        .count()
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let n = self.bar;
        self.bar += 1;
        let [high, low, _, _] = self.pivots.next(bar);
        let atr = self.atr.next(bar)[0];
        if !high.is_nan() {
            self.push(high, 1);
        }
        if !low.is_nan() {
            self.push(low, -1);
        }
        let fresh = !high.is_nan() || !low.is_nan();
        let count = self.points.len();
        if !fresh || count < 5 {
            return;
        }
        let [x, a, b, c, d] = std::array::from_fn(|k| self.points[count - 5 + k]);
        let bullish = d.1 == -1;
        let (xa, ab, bc) = ((a.0 - x.0).abs(), (b.0 - a.0).abs(), (c.0 - b.0).abs());
        let (cd, xd) = ((d.0 - c.0).abs(), (d.0 - x.0).abs());
        if !(xa > 0.0 && ab > 0.0 && bc > 0.0) {
            return;
        }
        let ratios = [ab / xa, bc / ab, cd / bc, xd / xa];
        let best = SHAPES.iter().map(|s| score(s, ratios)).max().expect("four shapes");
        let cooled = self.last_entry.is_none_or(|at| n - at >= COOLDOWN);
        if best < ENOUGH || !cooled {
            return;
        }
        let stop = if bullish { x.0 - STOP_ATRS * atr } else { x.0 + STOP_ATRS * atr };
        let risk = (d.0 - stop).abs();
        let target = if bullish { d.0 + REWARD * risk } else { d.0 - REWARD * risk };
        let (take, cut) = (target, stop);
        if bullish {
            strategy.entry(UP, Long).size(share_of_worth(&sizing, 10.0, bar.close));
            strategy.exit("long exit").from(UP).take(take).stop(cut);
        } else {
            strategy.entry(DOWN, Short).size(share_of_worth(&sizing, 10.0, bar.close));
            strategy.exit("short exit").from(DOWN).take(take).stop(cut);
        }
        self.last_entry = Some(n);
    }
}
