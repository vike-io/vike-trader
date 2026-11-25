// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Crossing Moving Averages with ADX Filter)" by BangtheClose, https://www.tradingview.com/script/gHYXDzo7/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2026-02-13, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Long all of the account's worth while SMA(20) of the close is above SMA(120) and the rule's own ADX(14) is at least 20,
//! short while it is below under the same filter, each entered only from flat or the other side, which it reverses. No costs.
//!
//! The header states no size and no capital. Under a 100% margin the engine's margin calls cut the shorts
//! (`[engine.margin_call]`), and the same margin refuses a reversal the next open has made worth more than the account.
//!
//! The ADX is written out in the rule, not the usual one: the first bar's directional moves are 0 and its true range is its
//! high less its low, so every Wilder average starts on the first bar, a bar earlier than the usual ADX
//! (`Adx::with_first_bar_range`); and nothing guards its divisions, so a line with nothing to divide by is missing and the
//! average of the DX starts over (`Adx::with_missing_ratios`).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Adx, Sma};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const LENGTH: f64 = 14.0;
const THRESHOLD: f64 = 20.0;

struct S {
    fast: Sma,
    slow: Sma,
    adx: Adx,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: Sma::new(20.0).expect("a valid length"),
        slow: Sma::new(120.0).expect("a valid length"),
        adx: Adx::new(LENGTH)
            .expect("a valid length")
            .with_first_bar_range(true)
            .with_missing_ratios(true),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let (fast, slow) = (self.fast.next(bar), self.slow.next(bar));
        let adx_ok = self.adx.next(bar)[0] >= THRESHOLD;

        let held = strategy.position();
        if fast > slow && adx_ok && held <= 0.0 {
            strategy.entry("long", Long).size(share_of_worth(&sizing, 100.0, bar.close));
        }
        if fast < slow && adx_ok && held >= 0.0 {
            strategy.entry("short", Short).size(share_of_worth(&sizing, 100.0, bar.close));
        }
    }
}
