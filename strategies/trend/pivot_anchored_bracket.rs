// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-trendmaster-pivot-anchored-bracket-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 1 when EMA(5) crosses up through EMA(13) and a pivot low exists, short 1 when it crosses down and a pivot high exists, an
//! opposite position reversing. While long, a stop at the last pivot low and a target twice that risk above the average cost; while
//! short, the mirror off the last pivot high. Both move every bar. A pivot has 5 bars on each side and is confirmed 5 bars after
//! it. No costs.
//!
//! The averages are vike's `ema` seeded with the first close (`with_seed_first`), not with the mean of the first closes.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::{ConfirmedPivot, Ema};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const LONG_LOT: &str = "long";
const SHORT_LOT: &str = "short";
const REWARD_PER_RISK: f64 = 2.0;

/// The last confirmed pivot of each side and the bracket they anchor, also copied into `combo_11`. NaN until a side's first pivot.
struct Anchors {
    pivots: ConfirmedPivot,
    high: f64,
    low: f64,
}

impl Anchors {
    fn new() -> Self {
        let pivots = ConfirmedPivot::new(5.0, 5.0).expect("valid spans");
        Anchors { pivots, high: f64::NAN, low: f64::NAN }
    }

    fn step(&mut self, bar: &Bar) {
        let [_, _, newest_high, newest_low] = self.pivots.next(bar);
        self.high = newest_high;
        self.low = newest_low;
    }

    /// Issues the bracket of whichever side is held.
    fn protect(&self, strategy: &mut Tactics<'_>) {
        let held = strategy.position();
        let cost = strategy.avg_cost();
        if held > 0.0 && !self.low.is_nan() {
            strategy
                .exit("long bracket")
                .from(LONG_LOT)
                .take(cost + (cost - self.low) * REWARD_PER_RISK)
                .stop(self.low);
        }
        if held < 0.0 && !self.high.is_nan() {
            strategy
                .exit("short bracket")
                .from(SHORT_LOT)
                .take(cost - (self.high - cost) * REWARD_PER_RISK)
                .stop(self.high);
        }
    }
}

/// An average seeded with the first value it sees.
fn from_first(length: f64) -> Ema {
    Ema::new(length).expect("a valid period").with_seed_first(true)
}

struct S {
    anchors: Anchors,
    fast: Ema,
    slow: Ema,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        anchors: Anchors::new(),
        fast: from_first(5.0),
        slow: from_first(13.0),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        self.anchors.step(bar);
        let fast = self.fast.next_value(bar.close);
        let slow = self.slow.next_value(bar.close);
        let held = strategy.position();
        match self.cross.step(fast, slow) {
            Signal::Long if !self.anchors.low.is_nan() && held <= 0.0 => {
                strategy.entry(LONG_LOT, Long).size(1.0);
            }
            Signal::Short if !self.anchors.high.is_nan() && held >= 0.0 => {
                strategy.entry(SHORT_LOT, Short).size(1.0);
            }
            _ => {}
        }
        self.anchors.protect(strategy);
    }
}
