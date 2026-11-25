// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Breakout Trend Follower" by millerrh, https://www.tradingview.com/script/9uvZIPnH/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2020-06-07, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! A swing breakout, long only. A pivot is a bar whose high (low) is the highest (lowest) of the 3 bars either side of it,
//! known 3 bars on; the levels are the last pivot high and the last pivot low. While the last pivot high lies over the SMA(50)
//! of the close, a buy stop rests at it, all of the account's worth; the stop is taken off the book on a bar the condition
//! fails. Once held (or while the entry rests), the exit is a stop at the last pivot low, moved as a new one comes.
//!
//! The rule's other filters (a daily MA, an RSI, an ADR) are off, and their higher-timeframe calls feed nothing. 0.1%
//! commission; a USD account on the USDT symbol, starting with 10,000.
//!
//! A stop's size spends the account's worth at its level on the grid, or at this close when the close is already through it.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{ConfirmedPivot, Sma};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::helpers::known;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

struct S {
    pivots: ConfirmedPivot,
    sma: Sma,
    high_level: f64,
    low_level: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        pivots: ConfirmedPivot::new(3.0, 3.0).expect("valid spans"),
        sma: Sma::new(50.0).expect("a valid length").with_fresh_sum(true),
        high_level: f64::NAN,
        low_level: f64::NAN,
    })
}

/// The price a stop entry at `level` is sized from.
fn sized_at(strategy: &Tactics<'_>, level: f64, close: f64) -> f64 {
    let level = strategy.snap_stop(Long, level);
    if level <= close { close } else { level }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.1), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let [high, low, _, _] = self.pivots.next(bar);
        if !high.is_nan() {
            self.high_level = high;
        }
        if !low.is_nan() {
            self.low_level = low;
        }
        let ma = self.sma.next(bar);
        if self.high_level > ma {
            let size =
                share_of_worth(&sizing, 100.0, sized_at(strategy, self.high_level, bar.close));
            strategy.entry("Long", Long).size(size).stop(self.high_level);
        } else {
            strategy.cancel("Long");
        }
        let exit = strategy.exit("Exit Long").from("Long");
        if let Some(cut) = known(self.low_level) {
            exit.stop(cut);
        }
    }
}
