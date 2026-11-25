// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-trendmaster-integration-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 1 when EMA(21) is above EMA(55) and that above EMA(200), RSI(14) is above 55, the close is above the previous bar's 20-bar
//! highest high and a pivot low exists. Short 1 on the mirror: the stack falling, RSI under 45, the close under the previous bar's
//! 20-bar lowest low, a pivot high. An opposite position reverses. While held, the bracket `pivot_anchored_bracket` issues. No costs.
//!
//! EMA(200) still remembers where it started.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::ConfirmedPivot;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const LONG_RSI: f64 = 55.0;
const SHORT_RSI: f64 = 45.0;

struct S {
    anchors: Anchors,
    fast: IndicatorLine,
    mid: IndicatorLine,
    slow: IndicatorLine,
    rsi: IndicatorLine,
    channel: IndicatorLine,
    highest_before: f64,
    lowest_before: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        anchors: Anchors::new(),
        fast: IndicatorLine::new("ema", &[21.0]),
        mid: IndicatorLine::new("ema", &[55.0]),
        slow: IndicatorLine::new("ema", &[200.0]),
        rsi: IndicatorLine::new("rsi", &[14.0]),
        channel: IndicatorLine::new("donchian", &[20.0]),
        highest_before: f64::NAN,
        lowest_before: f64::NAN,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        self.anchors.step(bar);
        let fast = self.fast.next(bar)[0];
        let mid = self.mid.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        let rsi = self.rsi.next(bar)[0];
        // The `donchian` row's lines are upper, mid, lower.
        let lines = self.channel.next(bar);
        let highest = std::mem::replace(&mut self.highest_before, lines[0]);
        let lowest = std::mem::replace(&mut self.lowest_before, lines[2]);

        let rising = fast > mid && mid > slow;
        let falling = fast < mid && mid < slow;
        let go_long = rising
            && fast > mid
            && rsi > LONG_RSI
            && bar.close > highest
            && !self.anchors.low.is_nan();
        let go_short = falling
            && fast < mid
            && rsi < SHORT_RSI
            && bar.close < lowest
            && !self.anchors.high.is_nan();

        let held = strategy.position();
        if go_long && held <= 0.0 {
            strategy.entry(LONG_LOT, Long).size(1.0);
        }
        if go_short && held >= 0.0 {
            strategy.entry(SHORT_LOT, Short).size(1.0);
        }
        self.anchors.protect(strategy);
    }
}

const LONG_LOT: &str = "long";
const SHORT_LOT: &str = "short";
const REWARD_PER_RISK: f64 = 2.0;

/// The last confirmed pivot of each side and the bracket they anchor, copied from `pivot_anchored_bracket`. NaN until a side's first pivot.
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
