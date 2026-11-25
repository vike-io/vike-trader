// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/udt-method-in-while-loop-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when at least 7 of the last 10 bars were green and EMA(9) crosses up through EMA(21), flat when it crosses down. A green
//! bar closes above its open. The up cross is only looked at on bars with those 7 greens, so its "previous bar" is the last such
//! bar, not always the one before. It never shorts.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Cross, Signal, SignalRule, SignalSource};

const LOOKBACK: usize = 10;

struct BackWalk {
    remaining: usize,
    greens: u32,
}

impl BackWalk {
    /// Visits the next bar, oldest first.
    fn visit(&mut self, was_green: &[bool; LOOKBACK]) {
        self.remaining -= 1;
        if was_green[self.remaining] {
            self.greens += 1;
        }
    }
}

struct S {
    /// Newest first. An unseen bar counts as not green.
    was_green: [bool; LOOKBACK],
    walk: BackWalk,
    fast: IndicatorLine,
    slow: IndicatorLine,
    gated_up: Cross,
    down: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        was_green: [false; LOOKBACK],
        walk: BackWalk { remaining: 0, greens: 0 },
        fast: IndicatorLine::new("ema", &[9.0]),
        slow: IndicatorLine::new("ema", &[21.0]),
        gated_up: Cross::new(),
        down: Cross::new(),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        self.was_green.rotate_right(1);
        self.was_green[0] = bar.close > bar.open;
        self.walk = BackWalk { remaining: LOOKBACK, greens: 0 };
        let mut visits = 0;
        while self.walk.remaining > 0 && visits < 12 {
            self.walk.visit(&self.was_green);
            visits += 1;
        }
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        let crossed_down = matches!(self.down.step(fast, slow), Signal::Short);
        if self.walk.greens >= 7 && matches!(self.gated_up.step(fast, slow), Signal::Long) {
            Signal::Long
        } else if crossed_down {
            Signal::Close
        } else {
            Signal::Hold
        }
    }
}
