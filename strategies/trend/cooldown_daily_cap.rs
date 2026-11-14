// SPDX-License-Identifier: Apache-2.0 AND MPL-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-ies-cooldown-daily-cap-01/strategy.pine
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived in part from "Integrated Execution System [JOAT]", © officialjackofalltrades, https://www.tradingview.com/script/SxASLpF5-Integrated-Execution-System-JOAT/
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when flat and EMA(9) crosses up through EMA(21), unless the last entry was fewer than 15 bars ago or 3 entries already
//! happened this UTC day. Close a long when EMA(9) crosses down. A refused cross is forgotten, not latched.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::Ema;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Cross, Signal, SignalRule, SignalSource};

const DAY_MS: i64 = 86_400_000;
const COOLDOWN_BARS: i64 = 15;
const ENTRIES_PER_DAY: i64 = 3;

struct S {
    fast: Ema,
    slow: Ema,
    cross: Cross,
    /// Counted from before the first bar, so the very first cross is not held back.
    bars_since_entry: i64,
    day: i64,
    entries_today: i64,
    /// The position this bar opened with: an order sent on a bar fills at the next bar's open.
    long: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        fast: Ema::new(9.0).expect("a valid period"),
        slow: Ema::new(21.0).expect("a valid period"),
        cross: Cross::new(),
        bars_since_entry: 999,
        day: i64::MIN,
        entries_today: 0,
        long: false,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        self.bars_since_entry += 1;
        let day = bar.timestamp.div_euclid(DAY_MS);
        if day != self.day {
            self.day = day;
            self.entries_today = 0;
        }
        match self.cross.step(self.fast.next(bar), self.slow.next(bar)) {
            Signal::Long
                if !self.long
                    && self.bars_since_entry >= COOLDOWN_BARS
                    && self.entries_today < ENTRIES_PER_DAY =>
            {
                self.long = true;
                self.bars_since_entry = 0;
                self.entries_today += 1;
                Signal::Long
            }
            Signal::Short if self.long => {
                self.long = false;
                Signal::Close
            }
            _ => Signal::Hold,
        }
    }
}
