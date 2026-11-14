// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "UT Bot v5" by SeaSide420, https://www.tradingview.com/script/2wkJx7Ix/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2021-11-08, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! A trailing stop 1 ATR(11) from the open that ratchets with it, and HMA(2) of the open: long all of the account's worth when
//! the open is above the stop and the average crosses up through it; a sell signal (the open under the stop and the stop
//! crossing up through the average) closes the long. Shorts are off. Any open profit over 50,000 or loss over 50,000 closes the
//! position. No costs; version 5 starts with 1,000,000.
//!
//! The rule's option to take its signals from Heikin-Ashi candles is off, so the candles are not built; its other averages
//! (SMA, EMA, WMA) are not chosen.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Atr, Hma};
use vike_indicators::strategy_kit::{max, min};
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::helpers::nan_to_zero;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "buy";
const LIMIT: f64 = 50_000.0;

struct S {
    atr: Atr,
    /// HMA(2): its half and its root are 1, so it is `2 * open - WMA(2)` of the opens.
    hma: Hma,
    stop: f64,
    previous_source: f64,
    above: Cross,
    below: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        atr: Atr::new(11.0).expect("a valid length"),
        hma: Hma::new(2.0).expect("a valid length").with_unit_lengths(true),
        stop: f64::NAN,
        previous_source: f64::NAN,
        above: Cross::new(),
        below: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { pyramiding: 2, ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let src = bar.open;
        let loss = self.atr.next(bar);
        let thema = self.hma.next_value(src);
        let last = nan_to_zero(self.stop);
        let previous_source = self.previous_source;
        let iff_1 = if src > last { src - loss } else { src + loss };
        let iff_2 =
            if src < last && previous_source < last { min(last, src + loss) } else { iff_1 };
        self.stop =
            if src > last && previous_source > last { max(last, src - loss) } else { iff_2 };
        self.previous_source = src;
        let above = matches!(self.above.step(thema, self.stop), Signal::Long);
        let below = matches!(self.below.step(self.stop, thema), Signal::Long);
        let buy = src > self.stop && above;
        let sell = src < self.stop && below;

        let held = strategy.position();
        let open_profit = (bar.close - strategy.avg_cost()) * held;
        let closing = held != 0.0 && open_profit.abs() > LIMIT;
        if closing {
            strategy.close_all();
        }
        if sell {
            strategy.close(NAME);
        }
        // A close placed ahead of an entry hands its pyramiding slot over (the limit is 1): the entry passes when the close
        // fills first, and is ignored when the position stays.
        if buy && (held == 0.0 || closing) {
            strategy.entry(NAME, Long).size(share_of_worth(&sizing, 100.0, bar.close));
        }
    }
}
