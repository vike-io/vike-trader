// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Elliott Wave with Supertrend Exit - Strategy [presentTrading]" by PresentTrading, https://www.tradingview.com/script/hlOgflm1/
// The original's notice: © PresentTrading
// Changed: rewritten by hand in Rust for vike.

//! A zigzag of pivots (4 bars before, 1 after) is read for five-wave motive patterns. A bearish pattern found at a pivot low
//! buys a tenth of the account's worth and a bullish one at a pivot high sells it, each reversing the other and each with a
//! stop 10% from this close, set again by every later signal on that side. The supertrend (3, ATR(10)) turning down closes the
//! long and turning up closes the short. 0.1% commission, 1 tick; starts with 10,000.
//!
//! The zigzag keeps the last eleven points, each a bar index, a price and a side. A pivot high of the side the last point
//! already holds replaces that point when higher; one of the other side is added. The six points read are the bar just made a
//! pivot and the five points after the newest (so a pivot high that did not replace the newest skips it). Bullish: wave 3
//! (points 4 - 3) is not the shortest of waves 1, 3 and 5, point 6 is above point 4, point 3 above point 1 and point 5 above
//! point 2; bearish mirrors it. A pattern whose first four points sit at the bar indices of the newest pattern found is that
//! pattern again, not a new one, and signals only when that pattern is of the same side.
//!
//! The supertrend is vike's `Supertrend` with the start that reads down (`Supertrend::with_down_start`) and the direction held
//! as the last bar left it (`Supertrend::with_held_direction`). The rule starts its bands at the first ATR bar's own; vike's
//! carries them there from 0, which gives the bar's own whenever the last close and the lower band are over 0, as on prices.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

use vike_indicators::indicators::{ConfirmedPivot, Supertrend};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;

const BUY: &str = "buy";
const SELL: &str = "sell";
const STOP: f64 = 10.0 / 100.0;
const POINTS: usize = 11;
const FACTOR: f64 = 3.0;

#[derive(Clone, Copy, Default)]
struct Point {
    side: i32,
    x: i64,
    y: f64,
}

/// A pattern found: the bar indices its first four points stood at, and its side.
struct Wave {
    xs: [i64; 4],
    side: i32,
}

struct S {
    pivots: ConfirmedPivot,
    zigzag: VecDeque<Point>,
    newest: Option<Wave>,
    index: i64,
    trend: Supertrend,
    last_direction: Option<i32>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        pivots: ConfirmedPivot::new(4.0, 1.0).expect("valid strengths"),
        zigzag: VecDeque::from(vec![Point::default(); POINTS]),
        newest: None,
        index: 0,
        trend: Supertrend::new(10.0, FACTOR)
            .expect("valid parameters")
            .with_down_start(true)
            .with_held_direction(true),
        last_direction: None,
    })
}

impl S {
    /// A pivot of `side` at bar `x` and price `y`: the zigzag moves, and `true` when it makes a pattern of that side.
    fn pivot(&mut self, side: i32, x: i64, y: f64) -> bool {
        let last = self.zigzag[0];
        if last.side * side < 1 {
            self.zigzag.push_front(Point { side, x, y });
            self.zigzag.pop_back();
        } else if (y - last.y) * f64::from(side) > 0.0 {
            self.zigzag[0] = Point { side, x, y };
        }
        let z = &self.zigzag;
        let p = [z[5], z[4], z[3], z[2], z[1], Point { side, x, y }];
        let s = f64::from(side);
        let wave = |a: usize, b: usize| (p[b].y - p[a].y) * s;
        let (w1, w3, w5) = (wave(0, 1), wave(2, 3), wave(4, 5));
        let is_wave = w3 != w1.min(w3).min(w5)
            && (p[5].y - p[3].y) * s > 0.0
            && (p[2].y - p[0].y) * s > 0.0
            && (p[4].y - p[1].y) * s > 0.0;
        let xs = [p[0].x, p[1].x, p[2].x, p[3].x];
        let same = self.newest.as_ref().is_some_and(|w| w.xs == xs);
        if is_wave && !same {
            self.newest = Some(Wave { xs, side });
        }
        is_wave && self.newest.as_ref().is_some_and(|w| w.side == side)
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.1), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let [high, low, _, _] = self.pivots.next(bar);
        let x = self.index - 1;
        let bullish = !high.is_nan() && self.pivot(1, x, high);
        let bearish = !low.is_nan() && self.pivot(-1, x, low);
        self.index += 1;
        if bearish {
            strategy.entry(BUY, Long).size(share_of_worth(
                &sizing,
                10.0,
                sizing.slipped(Long, bar.close),
            ));
            strategy.exit("exit long").from(BUY).stop(bar.close * (1.0 - STOP));
        }
        if bullish {
            strategy.entry(SELL, Short).size(share_of_worth(
                &sizing,
                10.0,
                sizing.slipped(Short, bar.close),
            ));
            strategy.exit("exit short").from(SELL).stop(bar.close * (1.0 + STOP));
        }
        // `1` down and `-1` up, as the reference reads it.
        let direction = if self.trend.next(bar)[1] > 0.0 { -1 } else { 1 };
        match self.last_direction.replace(direction) {
            Some(before) if before != direction && before < 0 => {
                strategy.close(BUY);
            }
            Some(before) if before != direction && before > 0 => {
                strategy.close(SELL);
            }
            _ => {}
        }
    }
}
