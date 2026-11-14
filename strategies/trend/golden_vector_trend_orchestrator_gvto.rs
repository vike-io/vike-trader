// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Golden Vector Trend Orchestrator (GVTO))" by Gustav_Rex, https://www.tradingview.com/script/dxhgQwyI/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2025-12-28, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Long 2% of the account's worth on the first bar that a supertrend (ATR(14) times 3.5) is up, the close is over EMA(200),
//! ADX(14, 14) is at least 25 and +DI is over -DI; short on the first bar of the mirror. A long closes while the supertrend
//! is down, a short while it is up. No costs; starts with 10,000 (stated in USD).
//!
//! The supertrend's bands are the bar's midpoint less and plus 3.5 ATR, each held at the previous band unless it moved
//! inward or the last close broke it; it turns up when the close goes over the upper band it was riding, down when under the
//! lower, and reads down until its ATR has a value behind it (`Supertrend::with_down_start`). The DMI averages the directional
//! moves and the true range with Wilder's average (seeded by a 14-bar mean, the first bar having neither), each DI held at its
//! last value while it has none (`Adx::with_held_lines`), and the ADX is Wilder's average of `|+DI - -DI| / (+DI + -DI)` times
//! 100.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Adx, Ema, Supertrend};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const SHARE: f64 = 2.0;
const FACTOR: f64 = 3.5;
const LENGTH: f64 = 14.0;
const ADX_MIN: f64 = 25.0;

struct S {
    ema: Ema,
    supertrend: Supertrend,
    dmi: Adx,
    long_was: bool,
    short_was: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        ema: Ema::new(200.0).expect("a valid length"),
        supertrend: Supertrend::new(LENGTH, FACTOR)
            .expect("valid parameters")
            .with_down_start(true),
        dmi: Adx::new(LENGTH).expect("a valid length").with_held_lines(true),
        long_was: false,
        short_was: false,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let ema = self.ema.next(bar);
        let [_, up] = self.supertrend.next(bar);
        let [adx, plus, minus] = self.dmi.next(bar);

        let (bull, bear) = (up > 0.0, up < 0.0);
        let strong = adx >= ADX_MIN;
        let long = bull && bar.close > ema && strong && plus > minus;
        let short = bear && bar.close < ema && strong && minus > plus;
        let (long_trigger, short_trigger) = (long && !self.long_was, short && !self.short_was);
        (self.long_was, self.short_was) = (long, short);

        if long_trigger {
            strategy.entry("Long Run", Long).size(share_of_worth(&sizing, SHARE, bar.close));
        }
        if short_trigger {
            strategy.entry("Short Run", Short).size(share_of_worth(&sizing, SHARE, bar.close));
        }
        if bear && strategy.position() > 0.0 {
            strategy.close("Long Run");
        }
        if bull && strategy.position() < 0.0 {
            strategy.close("Short Run");
        }
    }
}
