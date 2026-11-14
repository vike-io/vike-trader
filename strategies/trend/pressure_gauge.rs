// SPDX-License-Identifier: Apache-2.0 AND MPL-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-ies-pressure-gauge-01/strategy.pine
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived in part from "Integrated Execution System [JOAT]", © officialjackofalltrades, https://www.tradingview.com/script/SxASLpF5-Integrated-Execution-System-JOAT/
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Where each close sits in its bar's range, averaged by EMA(14) and then EMA(5), read two ways: its level on a ladder, and its
//! 10-bar change. Bullish at a level above 0.55 or a change above 0.05, bearish at a level below 0.45 or a change below -0.05.
//! Long on the bar bullish begins, short on the bar bearish begins. The only exit is the flip.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::Ema;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::flip_side;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

const MIDDLE: f64 = 0.5;
const BAND: f64 = 0.05;
const EXTREME_BUYING: f64 = 0.7;
const EXTREME_SELLING: f64 = 0.3;
const CHANGE_BARS: usize = 10;

/// The twice-averaged close position and its change, also copied into `combo_3`.
struct PressureChain {
    first: Ema,
    second: Ema,
    recent: [f64; CHANGE_BARS + 1],
}

impl PressureChain {
    fn new() -> Self {
        PressureChain {
            first: Ema::new(14.0).expect("a valid period"),
            // The second average starts on the first's first value: the first's warm-up NaNs are skipped, not pushed.
            second: Ema::new(5.0).expect("a valid period").with_skip_nan(true),
            recent: [f64::NAN; CHANGE_BARS + 1],
        }
    }
    /// The averaged position, then its change over `CHANGE_BARS`. A bar with no range sits in the middle.
    fn step(&mut self, bar: &Bar) -> (f64, f64) {
        let range = bar.high - bar.low;
        let position = if range > 0.0 { (bar.close - bar.low) / range } else { MIDDLE };
        let first = self.first.next_value(position);
        let second = self.second.next_value(first);
        self.recent.rotate_left(1);
        self.recent[CHANGE_BARS] = second;
        (second, second - self.recent[0])
    }
}

struct S {
    chain: PressureChain,
    was_bull: bool,
    was_bear: bool,
    held: i8,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        chain: PressureChain::new(),
        was_bull: false,
        was_bear: false,
        held: 0,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let (level, change) = self.chain.step(bar);
        let rung = if level >= EXTREME_BUYING {
            2
        } else if level > MIDDLE + BAND {
            1
        } else if level <= EXTREME_SELLING {
            -2
        } else if level < MIDDLE - BAND {
            -1
        } else {
            0
        };
        let bull = rung >= 1 || change > BAND;
        let bear = rung <= -1 || change < -BAND;
        let was_bull = std::mem::replace(&mut self.was_bull, bull);
        let was_bear = std::mem::replace(&mut self.was_bear, bear);
        flip_side(bull && !was_bull, bear && !was_bear, &mut self.held)
    }
}
