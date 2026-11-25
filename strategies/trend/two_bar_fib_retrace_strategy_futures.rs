// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Two-Bar Fib Retrace Strategy [Futures]" by UkesTrades, https://www.tradingview.com/script/NP615SiK/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2026-04-13, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Two bars, the one two bars back (bar 1) and the one before this (bar 2), each with a body of at least half its range, and a
//! rising volume from bar 1 to bar 2, make an impulse: both up, or bar 1 not both up with bar 2 closing over bar 1's high; down is
//! the mirror (bar 2 closing under bar 1's low). The close must lie over the day's VWAP of the typical price for a long, under it
//! for a short. This bar's low (high) must then reach back into the 50% to 61.8% retracement of bar 2's range. Long 1 (short 1)
//! at the next open, with a stop 1.0 beyond bar 2's low (high); a signal while that side is already held moves the stop. 2.5 a
//! contract on each fill, 1 tick, 100,000 start, and the margin of 100% a version-6 header without one carries.
//!
//! The rule's calls for the previous week's and days' levels, and its session levels, only draw. The VWAP restarts each UTC day.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Vwap, VwapSource};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const BODY_MIN: f64 = 0.5;
const SL_OFFSET: f64 = 1.0;

struct S {
    vwap: Vwap,
    /// The bars before this one, newest first: bar 2, then bar 1.
    before: [Option<Bar>; 2],
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        vwap: Vwap::default().with_source(VwapSource::TypicalDivided),
        before: [None, None],
    })
}

/// The share of a bar's range its body takes, 0 for a bar with no range.
fn body_share(b: &Bar) -> f64 {
    let range = b.high - b.low;
    if range != 0.0 { (b.close - b.open).abs() / range } else { 0.0 }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::per_contract(2.5), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let vwap = self.vwap.next(bar);
        let before = self.before.clone();
        self.before = [Some(bar.clone()), before[0].clone()];
        let [Some(b2), Some(b1)] = before else { return };
        let (b1_bull, b1_bear) = (b1.close > b1.open, b1.close < b1.open);
        let (b2_bull, b2_bear) = (b2.close > b2.open, b2.close < b2.open);
        let full = body_share(&b1) >= BODY_MIN && body_share(&b2) >= BODY_MIN;
        let volume_up = b2.volume > b1.volume;
        let base = full && volume_up;
        let bull_impulse =
            base && ((b1_bull && b2_bull) || (!(b1_bull && b2_bull) && b2.close > b1.high));
        let bear_impulse =
            base && ((b1_bear && b2_bear) || (!(b1_bear && b2_bear) && b2.close < b1.low));
        let range = b2.high - b2.low;
        let bull_zone = (b2.high - range * 0.618, b2.high - range * 0.5);
        let bear_zone = (b2.low + range * 0.5, b2.low + range * 0.618);
        let bull =
            bull_impulse && bar.close > vwap && bar.low <= bull_zone.1 && bar.low >= bull_zone.0;
        let bear =
            bear_impulse && bar.close < vwap && bar.high >= bear_zone.0 && bar.high <= bear_zone.1;
        if bull {
            strategy.entry("Long", Long).size(1.0);
            strategy.exit("L-SL").from("Long").stop(b2.low - SL_OFFSET);
        }
        if bear {
            strategy.entry("Short", Short).size(1.0);
            strategy.exit("S-SL").from("Short").stop(b2.high + SL_OFFSET);
        }
    }
}
