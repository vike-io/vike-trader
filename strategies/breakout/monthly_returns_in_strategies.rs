// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from the script by QuantNomad, https://www.tradingview.com/script/kzp8e4X3/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2021-08-16, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Pivots of 2 bars before and 1 after, on the highs and on the lows. From a pivot high until a high goes above it, every bar
//! places a buy stop a tick over it; from a pivot low until a low goes under it, a sell stop a tick under it. Each is 25% of the
//! account's worth and reverses the other side; an order not placed again rests as it was. 0.1% commission; starts with
//! 100,000.
//!
//! A stop's size spends the account's worth at its level on the grid, or at this close when the close is already through it.
//! The rule's monthly and yearly return tables only draw, and its recalculation on every tick is the close on a closed bar.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::ConfirmedPivot;
use vike_marketdata::Bar;
use vike_model::OrderSide;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const UP: &str = "PivRevLE";
const DOWN: &str = "PivRevSE";
const SHARE: f64 = 25.0;

struct S {
    pivots: ConfirmedPivot,
    high: f64,
    low: f64,
    long_armed: bool,
    short_armed: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        pivots: ConfirmedPivot::new(2.0, 1.0).expect("valid spans"),
        high: 0.0,
        low: 0.0,
        long_armed: false,
        short_armed: false,
    })
}

/// The price a stop entry at `level` is sized from.
fn sized_at(strategy: &Tactics<'_>, side: OrderSide, level: f64, close: f64) -> f64 {
    let level = strategy.snap_stop(side, level);
    if (side == Long && level <= close) || (side == Short && level >= close) {
        close
    } else {
        level
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.1), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let sizing = strategy.sizing();
        let [pivot_high, pivot_low, _, _] = self.pivots.next(bar);
        if !pivot_high.is_nan() {
            self.high = pivot_high;
            self.long_armed = true;
        } else if self.long_armed && bar.high > self.high {
            self.long_armed = false;
        }
        if !pivot_low.is_nan() {
            self.low = pivot_low;
            self.short_armed = true;
        } else if self.short_armed && bar.low < self.low {
            self.short_armed = false;
        }
        if self.long_armed {
            let level = self.high + tick;
            let size = share_of_worth(&sizing, SHARE, sized_at(strategy, Long, level, bar.close));
            strategy.entry(UP, Long).size(size).stop(level);
        }
        if self.short_armed {
            let level = self.low - tick;
            let size = share_of_worth(&sizing, SHARE, sized_at(strategy, Short, level, bar.close));
            strategy.entry(DOWN, Short).size(size).stop(level);
        }
    }
}
