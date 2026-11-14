// SPDX-License-Identifier: Apache-2.0 AND MPL-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-ies-three-ema-bias-score-01/strategy.pine
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived in part from "Integrated Execution System [JOAT]", © officialjackofalltrades, https://www.tradingview.com/script/SxASLpF5-Integrated-Execution-System-JOAT/
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Scores EMA(21), EMA(55) and EMA(200) for each side: 30 for all three in order, 20 for a close beyond both faster averages, 20 for
//! a close beyond the slowest. A side has bias at 30 or more. Long on the bar bullish bias begins, short on the bar bearish bias
//! begins; both can hold at once, and then the side held decides. The only exit is the flip.
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

const ORDER_POINTS: u8 = 30;
const STRUCTURE_POINTS: u8 = 20;
const TREND_POINTS: u8 = 20;
const BIAS_POINTS: u8 = 30;

/// The score of one side. A NaN average earns nothing.
fn points(in_order: bool, beyond_structure: bool, beyond_trend: bool) -> u8 {
    u8::from(in_order) * ORDER_POINTS
        + u8::from(beyond_structure) * STRUCTURE_POINTS
        + u8::from(beyond_trend) * TREND_POINTS
}

/// Whether each side has bias, also copied into `combo_3`.
struct BiasScore {
    averages: [Ema; 3],
}

impl BiasScore {
    fn new() -> Self {
        BiasScore { averages: [21.0, 55.0, 200.0].map(|n| Ema::new(n).expect("a valid period")) }
    }
    /// Bullish bias, then bearish bias.
    fn step(&mut self, bar: &Bar) -> (bool, bool) {
        let [fast, slow, trend] = [0, 1, 2].map(|i| self.averages[i].next(bar));
        let close = bar.close;
        let bull = points(fast > slow && slow > trend, close > fast && close > slow, close > trend);
        let bear = points(fast < slow && slow < trend, close < fast && close < slow, close < trend);
        (bull >= BIAS_POINTS, bear >= BIAS_POINTS)
    }
}

struct S {
    score: BiasScore,
    was_bull: bool,
    was_bear: bool,
    held: i8,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S { score: BiasScore::new(), was_bull: false, was_bear: false, held: 0 }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let (bull, bear) = self.score.step(bar);
        let was_bull = std::mem::replace(&mut self.was_bull, bull);
        let was_bear = std::mem::replace(&mut self.was_bear, bear);
        flip_side(bull && !was_bull, bear && !was_bear, &mut self.held)
    }
}
