// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Ajay R5.41" by Ajay_Janjgir, https://www.tradingview.com/script/mBhnRO3S/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence, so TradingView's Terms of Use make it MPL-2.0.

//! An ALMA (2 bars, offset 0.85, sigma 5) of the close and one of the open, both taken on 240-minute bars and read on each
//! 30-minute bar. Long 10% of the account's worth when the close's ALMA crosses up through the open's, short 10% when it
//! crosses down, each reversing the other. No costs; starts with 1,000,000.
//!
//! The rule reads the 240-minute bar with lookahead on (`timeframe::Higher::looking_ahead`): from the first 30-minute bar of a
//! 4-hour period it sees that period's final open and close, which looks ahead of the bar. An intraday frame has no bar before
//! the window (`crate::timeframe`'s rule 2), so the first period has no value and the first cross waits for the second. Fed the
//! venue's bar before the window as well, the rule has 1871 trades, one too many, and misses by 1243; from the window alone
//! it has 1870 and the reference's net. Its Heikin-Ashi source is off by default, and everything else it draws feeds no order.

use crate::desk::{Plan, Terms};
use crate::timeframe::Higher;
use vike_indicators::indicators::Alma;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_tactics::HigherTimeframePeriod;
use vike_tactics::Tactics;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;

struct S {
    fours: Higher,
    close: Alma,
    open: Alma,
    cross: Cross,
    /// The two ALMAs of the 4-hour bar in hand.
    last: (f64, f64),
}

pub fn new() -> Box<dyn Plan> {
    let alma = || Alma::new(2.0, 0.85, 5.0).expect("valid parameters");
    Box::new(S {
        fours: Higher::looking_ahead(HigherTimeframePeriod::Minutes(240)),
        close: alma(),
        open: alma(),
        cross: Cross::new(),
        last: (f64::NAN, f64::NAN),
    })
}

impl Plan for S {
    fn terms(&self) -> Terms {
        Terms { cash: 1_000_000.0, ..Terms::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        for four in self.fours.next(bar) {
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
