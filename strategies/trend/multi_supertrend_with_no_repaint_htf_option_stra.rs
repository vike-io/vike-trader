// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Multi Supertrend with no-repaint HTF option strategy" by ramki_simple, https://www.tradingview.com/script/DrNY2LOA/
// The original's notice: © ramki_simple
// Changed: rewritten by hand in Rust for vike.

//! Two supertrends on the chart, (10, 3) and (14, 2): long 1 on the bar both first read up, short 1 on the bar both first
//! read down, each reversing the other; a long is closed on the bar the two stop both reading up, a short on the bar they stop
//! both reading down. No costs; starts with 100,000.
//!
//! The second supertrend is meant for a higher timeframe, read with lookahead from the bar before; the rule's default leaves
//! that timeframe empty, so it runs on the chart and nothing looks ahead. Each supertrend starts reading down
//! (`Supertrend::with_down_start`). Both read down on the first bar, whose "bar before" is missing, and `not` of a missing value
//! is true: so the first bar enters a short.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Supertrend;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

struct S {
    first: Supertrend,
    second: Supertrend,
    /// Last bar's `(long, short)`; before the first bar a missing value, which `not` reads as true.
    was: (bool, bool),
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        first: Supertrend::new(10.0, 3.0).expect("valid parameters").with_down_start(true),
        second: Supertrend::new(14.0, 2.0).expect("valid parameters").with_down_start(true),
        was: (false, false),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        // The rule's directions: 1 down, -1 up.
        let (a, b) = (-self.first.next(bar)[1], -self.second.next(bar)[1]);
        let long = a == -1.0 && b == -1.0;
        let short = a == 1.0 && b == 1.0;
        let (was_long, was_short) = self.was;
        if long && !was_long {
            strategy.entry("enter long", Long).size(1.0);
        }
        if short && !was_short {
            strategy.entry("enter short", Short).size(1.0);
        }
        if was_long && !long {
            strategy.close("enter long");
        }
        if was_short && !short {
            strategy.close("enter short");
        }
        self.was = (long, short);
    }
}
