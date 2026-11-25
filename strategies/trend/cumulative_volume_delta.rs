// SPDX-License-Identifier: Apache-2.0 AND MPL-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-vcp-cumulative-volume-delta-01/strategy.pine
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived in part from "Vortex Confluence Protocol [JOAT]", © officialjackofalltrades, https://www.tradingview.com/script/XtP27UK6-Vortex-Confluence-Protocol-JOAT/
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long only, on a rolling 10-bar sum of each bar's volume delta, split by where the close sits in the bar's range. Buy when the
//! sum crosses up through zero, close when it crosses down through zero.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Cross, Signal, SignalRule, SignalSource};

const WINDOW: usize = 10;
/// Added to the bar's range so that a bar with no range does not divide by zero.
const RANGE_PAD: f64 = 0.0001;

/// Vike has no row for this: `net_volume` signs a whole bar's volume by its close move. A bar closing above its open counts all
/// its volume as bought and a bar closing below as sold; the other side, and both sides of a doji, are the share of the volume
/// the close's place in the range gives.
fn volume_delta(bar: &Bar) -> f64 {
    let range = bar.high - bar.low + RANGE_PAD;
    let bought =
        if bar.close > bar.open { bar.volume } else { bar.volume * (bar.close - bar.low) / range };
    let sold =
        if bar.close < bar.open { bar.volume } else { bar.volume * (bar.high - bar.close) / range };
    bought - sold
}

struct S {
    // Vike has no rolling-sum row either. Oldest first, and NaN until full, so the sum is NaN until then.
    deltas: [f64; WINDOW],
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S { deltas: [f64::NAN; WINDOW], cross: Cross::new() }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        self.deltas.rotate_left(1);
        self.deltas[WINDOW - 1] = volume_delta(bar);
        match self.cross.step(self.deltas.iter().sum(), 0.0) {
            Signal::Short => Signal::Close,
            other => other,
        }
    }
}
