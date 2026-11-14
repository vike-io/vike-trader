// SPDX-License-Identifier: Apache-2.0 AND MPL-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-ies-rsi-macd-momentum-01/strategy.pine
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived in part from "Integrated Execution System [JOAT]", © officialjackofalltrades, https://www.tradingview.com/script/SxASLpF5-Integrated-Execution-System-JOAT/
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Three votes a side from RSI(14) and MACD(12,26,9): RSI beyond 55 or 45, RSI moved that way over 3 bars, and the histogram on that
//! side of zero and growing away from it. Long on the bar the bull votes reach 2, short on the bar the bear votes reach 2. The only
//! exit is the flip.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

const RSI_BULL: f64 = 55.0;
const RSI_BEAR: f64 = 45.0;
const RSI_LOOKBACK: usize = 3;
const VOTES_NEEDED: u8 = 2;

/// The two vote counts, also copied into `combo_3`.
struct MomentumVotes {
    rsi: IndicatorLine,
    macd: IndicatorLine,
    recent_rsi: [f64; RSI_LOOKBACK + 1],
    previous_hist: f64,
}

impl MomentumVotes {
    fn new() -> Self {
        MomentumVotes {
            rsi: IndicatorLine::new("rsi", &[14.0]),
            macd: IndicatorLine::new("macd", &[12.0, 26.0, 9.0]),
            recent_rsi: [f64::NAN; RSI_LOOKBACK + 1],
            previous_hist: f64::NAN,
        }
    }
    /// The bull votes, then the bear votes.
    fn step(&mut self, bar: &Bar) -> (u8, u8) {
        let rsi = self.rsi.next(bar)[0];
        self.recent_rsi.rotate_left(1);
        self.recent_rsi[RSI_LOOKBACK] = rsi;
        let then = self.recent_rsi[0];
        // The `macd` row's lines are macd, signal, hist.
        let hist = self.macd.next(bar)[2];
        let previous_hist = std::mem::replace(&mut self.previous_hist, hist);

        let bull = [rsi > RSI_BULL, rsi > then, hist > 0.0 && hist > previous_hist];
        let bear = [rsi < RSI_BEAR, rsi < then, hist < 0.0 && hist < previous_hist];
        let count = |votes: [bool; 3]| votes.iter().filter(|&&v| v).count() as u8;
        (count(bull), count(bear))
    }
}

struct S {
    votes: MomentumVotes,
    was_bull: bool,
    was_bear: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S { votes: MomentumVotes::new(), was_bull: false, was_bear: false }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let (bull, bear) = self.votes.step(bar);
        let (bull, bear) = (bull >= VOTES_NEEDED, bear >= VOTES_NEEDED);
        let was_bull = std::mem::replace(&mut self.was_bull, bull);
        let was_bear = std::mem::replace(&mut self.was_bear, bear);
        // Each vote excludes its opposite, so three votes cannot make 2 on both sides at once.
        if bull && !was_bull {
            Signal::Long
        } else if bear && !was_bear {
            Signal::Short
        } else {
            Signal::Hold
        }
    }
}
