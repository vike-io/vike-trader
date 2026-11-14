// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/analyzer-parity-choch-bos-isolator-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Market-structure breaks from pivots with 5 bars on each side. A close beyond the latest pivot, when structure is not already that
//! way, or beyond the pivot before it, when structure is the other way, sets the structure and, when flat, opens a position of 1
//! that way. A position is closed from the first bar after its entry bar on, so it fills two opens later. No costs.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::ConfirmedPivot;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const SIZE: f64 = 1.0;

/// One side's pivots. NaN until seen, and a NaN compares false, so an absent pivot breaks nothing.
#[derive(Clone, Copy)]
struct Swings {
    latest: f64,
    before: f64,
}

impl Swings {
    /// `pivot` is the one this bar confirms, NaN when none.
    fn note(&mut self, pivot: f64) {
        if !pivot.is_nan() {
            self.before = self.latest;
            self.latest = pivot;
        }
    }
}

struct S {
    pivots: ConfirmedPivot,
    highs: Swings,
    lows: Swings,
    // 1 up, -1 down, 0 until the first break.
    structure: i32,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let unseen = Swings { latest: f64::NAN, before: f64::NAN };
    let pivots = ConfirmedPivot::new(5.0, 5.0).expect("valid spans");
    Box::new(S { pivots, highs: unseen, lows: unseen, structure: 0 })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let [pivot_high, pivot_low, ..] = self.pivots.next(bar);
        self.highs.note(pivot_high);
        self.lows.note(pivot_low);

        let up = (bar.close > self.highs.latest && self.structure <= 0)
            || (bar.close > self.highs.before && self.structure < 0);
        let down = (bar.close < self.lows.latest && self.structure >= 0)
            || (bar.close < self.lows.before && self.structure > 0);
        if up {
            self.structure = 1;
        }
        if down {
            self.structure = -1;
        }

        let held = strategy.position();
        if up && held == 0.0 {
            strategy.entry("up", Long).size(SIZE);
        }
        if down && held == 0.0 {
            strategy.entry("down", Short).size(SIZE);
        }
        if held != 0.0
            && strategy.lots().first().is_some_and(|lot| bar.timestamp > lot.entry_timestamp)
        {
            strategy.close_all();
        }
    }
}
