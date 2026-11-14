// SPDX-License-Identifier: Apache-2.0 AND MPL-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-ies-pivot-liquidity-sweep-01/strategy.pine
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived in part from "Integrated Execution System [JOAT]", © officialjackofalltrades, https://www.tradingview.com/script/SxASLpF5-Integrated-Execution-System-JOAT/
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when a bar's low pokes under the last confirmed swing low and it closes back above it, short when its high pokes over the
//! last confirmed swing high and it closes back below it. A swing has 10 bars before it and 5 after, and is confirmed on the 5th
//! bar after. The only exit is the flip.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::ConfirmedPivot;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::flip_side;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

const BEFORE: f64 = 10.0;
const AFTER: f64 = 5.0;

struct S {
    /// Vike's `confirmed_pivot`: bars before the candidate may tie it, bars after it must lose strictly, as in
    /// `pivot_bos_trigger`.
    swings: ConfirmedPivot,
    held: i8,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        swings: ConfirmedPivot::new(BEFORE, AFTER).expect("valid spans"),
        held: 0,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        // The newest confirmed swing of each side, NaN until the first.
        let [_, _, swing_high, swing_low] = self.swings.next(bar);
        let swept_low = bar.low < swing_low && bar.close > swing_low;
        let swept_high = bar.high > swing_high && bar.close < swing_high;
        flip_side(swept_low, swept_high, &mut self.held)
    }
}
