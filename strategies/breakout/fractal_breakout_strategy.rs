// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Fractal Breakout Strategy" by ceyhun, https://www.tradingview.com/script/Mwu4JMaQ/
// The original's notice: © ceyhun
// Changed: rewritten by hand in Rust for vike.

//! An up fractal is a high two bars back above the two highs after it and above the highs before it: the two just before it
//! strictly lower, and up to four before those no higher (a window of 5 to 9 bars); a down fractal is the mirror on the lows.
//! The levels in force are the fractal before the latest one of each kind. Long 1 when the close crosses up through the up
//! level, short 1 when it crosses down through the down level, each reversing the other; on a bar with both, the long
//! fills first and the short then reverses it. No costs; starts with 100,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

/// The newest value first, `NaN` before the bars came.
type Ring = [f64; 9];

struct S {
    highs: Ring,
    lows: Ring,
    /// `(latest, the one before)` of each kind.
    up: (f64, f64),
    down: (f64, f64),
    cross_up: Cross,
    cross_down: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        highs: [f64::NAN; 9],
        lows: [f64::NAN; 9],
        up: (f64::NAN, f64::NAN),
        down: (f64::NAN, f64::NAN),
        cross_up: Cross::new(),
        cross_down: Cross::new(),
    })
}

fn push(ring: &mut Ring, x: f64) {
    ring.rotate_right(1);
    ring[0] = x;
}

/// Whether the value two bars back stands out of `ring` by `beyond`: `beyond(a, b)` is `a` strictly past `b`.
// Not vike's `ConfirmedPivot`: the two bars just before must lose strictly and up to four before them may tie, a window of 5
// to 9 bars that no fixed `left` gives.
fn fractal(ring: &Ring, beyond: fn(f64, f64) -> bool) -> bool {
    let mid = ring[2];
    let clear = |x: f64| beyond(mid, x);
    let level = |x: f64| !beyond(x, mid) && !x.is_nan() && !mid.is_nan();
    clear(ring[1])
        && clear(ring[0])
        && (4..=8).any(|j| {
            clear(ring[j]) && clear(ring[j - 1]) && ring[3..=j - 2].iter().all(|&x| level(x))
        })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        push(&mut self.highs, bar.high);
        push(&mut self.lows, bar.low);
        if fractal(&self.highs, |a, b| a > b) {
            self.up = (self.highs[2], self.up.0);
        }
        if fractal(&self.lows, |a, b| a < b) {
            self.down = (self.lows[2], self.down.0);
        }
        let up = matches!(self.cross_up.step(bar.close, self.up.1), Signal::Long);
        let down = matches!(self.cross_down.step(bar.close, self.down.1), Signal::Short);
        // On a bar with both, the long fills first and the engine sizes the short against the position it leaves.
        if up {
            strategy.entry("long", Long).size(1.0);
        }
        if down {
            strategy.entry("short", Short).size(1.0);
        }
    }
}
