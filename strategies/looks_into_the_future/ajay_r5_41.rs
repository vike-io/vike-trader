// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Ajay R5.41" by Ajay_Janjgir, https://www.tradingview.com/script/mBhnRO3S/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2025-10-06, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! An ALMA (2 bars, offset 0.85, sigma 5) of the close and one of the open, both taken on 240-minute bars and read on each
//! 30-minute bar. Long 10% of the account's worth when the close's ALMA crosses up through the open's, short 10% when it
//! crosses down, each reversing the other. No costs; starts with 1,000,000.
//!
//! **It looks into the future.** The rule reads the 240-minute bar with lookahead on: from the first 30-minute bar of a 4-hour
//! period it sees that period's FINAL open and close, which no live run can know
//! (`vike_tactics::higher::HigherTimeframeSourceAhead`, over the run's whole 30-minute series). So it runs in a backtest only,
//! and its backtest is better than reality. The frame starts at the rule's first bar (a frame of minutes has no bar before
//! the window), so the first cross waits for the second period. Fed the venue's bar before the window as well, the rule has
//! 1871 trades, one too many, and misses by 1243; from the window alone it has 1870 and the reference's net. Its Heikin-Ashi
//! source is off by default, and everything else it draws feeds no order.
//!
//! ```toml
//! [strategy]
//! runs_on = "backtest"
//! why = "looks into the future: reads the 240-minute bar in progress at its final values; a backtest is better than reality"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Alma;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::higher::HigherTimeframeSourceAhead;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{HigherTimeframePeriod, Rule, RuleDefaults, Tactics};

struct S {
    fours: HigherTimeframeSourceAhead,
    close: Alma,
    open: Alma,
    cross: Cross,
    /// The two ALMAs of the 4-hour bar in hand.
    last: (f64, f64),
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let alma = || Alma::new(2.0, 0.85, 5.0).expect("valid parameters");
    Box::new(S {
        fours: HigherTimeframeSourceAhead::new(HigherTimeframePeriod::Minutes(240)),
        close: alma(),
        open: alma(),
        cross: Cross::new(),
        last: (f64::NAN, f64::NAN),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn inputs(&self) -> Vec<String> {
        vec![self.fours.input().to_string()]
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        for four in self.fours.next(bar, strategy) {
            self.last = (self.close.next_value(four.close), self.open.next_value(four.open));
        }
        match self.cross.step(self.last.0, self.last.1) {
            Signal::Long => {
                strategy.entry("Long", Long).size(share_of_worth(&sizing, 10.0, bar.close));
            }
            Signal::Short => {
                strategy.entry("Short", Short).size(share_of_worth(&sizing, 10.0, bar.close));
            }
            _ => {}
        }
    }
}
