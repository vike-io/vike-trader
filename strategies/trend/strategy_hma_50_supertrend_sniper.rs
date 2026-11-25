// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Strategy: HMA 50 + Supertrend Sniper" by Gustav_Rex, https://www.tradingview.com/script/3SHJZytF/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2025-12-02, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Long all of the account's worth while HMA(50) of the close rises and Supertrend(10, 3) is up; short all of it while the
//! HMA falls and the Supertrend is down, each reversing the other. Everything is closed when the two disagree. No costs;
//! starts with 10,000.
//!
//! The Supertrend is the reference's: vike's `Supertrend` with the start that reads down (`Supertrend::with_down_start`: the
//! ATR from the first bar's range, down through the first bar with an ATR, the first bands carried from 0) and the direction
//! held as the last bar left it (`Supertrend::with_held_direction`), which carries a band at an equality and turns only past
//! one. The rule's date filter is off.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Hma, Supertrend};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

struct S {
    hma: Hma,
    hma_previous: f64,
    trend: Supertrend,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        hma: Hma::new(50.0).expect("a valid length"),
        hma_previous: f64::NAN,
        trend: Supertrend::new(10.0, 3.0)
            .expect("valid parameters")
            .with_down_start(true)
            .with_held_direction(true),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let hma = self.hma.next(bar);
        let rising = hma > self.hma_previous;
        let falling = hma < self.hma_previous;
        self.hma_previous = hma;
        // `-1` up and `1` down, as the reference reads it.
        let direction = if self.trend.next(bar)[1] > 0.0 { -1 } else { 1 };
        if rising && direction == -1 {
            strategy.entry("long", Long).size(share_of_worth(&sizing, 100.0, bar.close));
        }
        if falling && direction == 1 {
            strategy.entry("short", Short).size(share_of_worth(&sizing, 100.0, bar.close));
        }
        if (rising && direction == 1) || (falling && direction == -1) {
            strategy.close_all();
        }
    }
}
