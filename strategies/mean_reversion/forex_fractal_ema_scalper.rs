// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Forex Fractal EMA Scalper" by exlux, https://www.tradingview.com/script/pgsJGhIg/
// The original's notice: © exlux99
// Changed: rewritten by hand in Rust for vike.

//! Three EMAs of the bar's midpoint, 10, 20 and 100 bars. When they stand fast over middle over slow and an up fractal has formed,
//! short 1; when they stand the other way round and a down fractal has formed, long 1: the rule's names are swapped and it trades
//! them as written. Either reverses the other side. Each position leaves at a target or a stop 250 ticks from its entry fill.
//! No costs.
//!
//! An up fractal is a high two bars back over the two after it, and over the two before it, of which up to four nearer ones may tie
//! it first; a down fractal is the mirror on the lows.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use vike_tactics::helpers::back;
use vike_tactics::{Rule, RuleDefaults, Tactics, TacticsTrail};

use vike_indicators::indicators::Ema;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;

const UP: &str = "long";
const DOWN: &str = "short";
const TICKS: f64 = 250.0;
/// The rule's exit is one order with a target and a stop, issued as two guards of one group: alternatives, which hold none of
/// the entry's quantity against one another.
const LONG_EXIT: &str = "long exit";
const SHORT_EXIT: &str = "short exit";
/// The target: the first touch 250 ticks past the entry fill, which the engine rests from the fill.
const TARGET: TacticsTrail = TacticsTrail { after_ticks: TICKS, by_ticks: 0.0 };
/// The bars a fractal reads, the newest first.
const SPAN: usize = 9;

/// A value two bars back that the two after it lose to (`loses(other, c)`), and before it `k` bars, 0 to 4, that lose or tie and
/// then two that lose. A NaN neither loses nor ties.
// Not vike's `ConfirmedPivot`: the window is 5 to 9 bars as the ties before the two losers allow, no fixed `left` gives it.
fn fractal(xs: &VecDeque<f64>, loses: impl Fn(f64, f64) -> bool) -> bool {
    let c = back(xs, 2);
    let x = |i: usize| back(xs, i);
    let loses_at = |i: usize| loses(x(i), c);
    let after = loses_at(1) && loses_at(0);
    let before = (0..=4).any(|k| {
        (3..3 + k).all(|i| loses_at(i) || x(i) == c) && loses_at(3 + k) && loses_at(4 + k)
    });
    after && before
}

struct S {
    fast: Ema,
    middle: Ema,
    slow: Ema,
    highs: VecDeque<f64>,
    lows: VecDeque<f64>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: Ema::new(10.0).expect("a valid length"),
        middle: Ema::new(20.0).expect("a valid length"),
        slow: Ema::new(100.0).expect("a valid length"),
        highs: VecDeque::with_capacity(SPAN + 1),
        lows: VecDeque::with_capacity(SPAN + 1),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        if strategy.tick().is_none() {
            return; // its target and stop count in ticks
        }
        let mid = (bar.high + bar.low) / 2.0;
        let fast = self.fast.next_value(mid);
        let middle = self.middle.next_value(mid);
        let slow = self.slow.next_value(mid);
        self.highs.push_front(bar.high);
        self.highs.truncate(SPAN);
        self.lows.push_front(bar.low);
        self.lows.truncate(SPAN);
        let up_fractal = fractal(&self.highs, |other, c| other < c);
        let down_fractal = fractal(&self.lows, |other, c| other > c);
        let rising = fast > middle && middle > slow && up_fractal;
        let falling = fast < middle && middle < slow && down_fractal;
        // A side already held takes no second lot, and its exits stay where its fill put them.
        let held = strategy.position();
        if falling && held <= 0.0 {
            strategy.entry(UP, Long).size(1.0);
            strategy.exit("X_long").from(UP).trail(TARGET).group(LONG_EXIT);
        }
        if rising && held >= 0.0 {
            strategy.entry(DOWN, Short).size(1.0);
            strategy.exit("x_short").from(DOWN).trail(TARGET).group(SHORT_EXIT);
        }
    }

    /// The stop works from the entry's fill, so it is set once the entry has filled, in place of its stand-in.
    fn after_fill(&mut self, strategy: &mut Tactics<'_>, _bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let width = TICKS * tick;
        let held = strategy.position();
        let cost = strategy.avg_cost();
        if held > 0.0 {
            strategy.exit("X_long stop").from(UP).stop(cost - width).group(LONG_EXIT);
        } else if held < 0.0 {
            strategy.exit("x_short stop").from(DOWN).stop(cost + width).group(SHORT_EXIT);
        }
    }
}
