// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Inside Bar Strategy w/ SL" by tweakerID, https://www.tradingview.com/script/JeW4LqUA/
// The original's notice: © tweakerID
// Changed: rewritten by hand in Rust for vike.

//! An inside bar (a high under the bar before's and a low over it) that closes up while the close is over SMA(65) is a long
//! signal, one that closes down while the close is not over it a short; from the 42nd bar on. Each enters all of the
//! account's worth, reversing the other. A position's stop is five ATR(14) from the bar's OHLC average on the bar its
//! position changed, its target twice that distance on the other side of the entry price. 0.04% commission; starts with 10,000.
//!
//! The rule's stop and target are issued every bar a position is held, from the bar after its entry: on the entry bar the
//! position the levels read was not yet held. Its direction setting allows both sides.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Atr, Sma};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const ATR_MULTIPLIER: f64 = 5.0;
const REWARD: f64 = 2.0;

struct S {
    ma: Sma,
    atr: Atr,
    previous: Option<Bar>,
    bars: usize,
    previous_held: f64,
    /// The OHLC average less, and plus, the ATR distance on the last bar the position changed.
    long_stop: f64,
    short_stop: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        ma: Sma::new(65.0).expect("a valid length"),
        atr: Atr::new(14.0).expect("a valid length"),
        previous: None,
        bars: 0,
        previous_held: 0.0,
        long_stop: f64::NAN,
        short_stop: f64::NAN,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.04), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let ma = self.ma.next(bar);
        let distance = self.atr.next(bar) * ATR_MULTIPLIER;
        let index = self.bars;
        self.bars += 1;
        let held = strategy.position();
        if held != self.previous_held {
            let ohlc4 = (bar.open + bar.high + bar.low + bar.close) / 4.0;
            self.long_stop = ohlc4 - distance;
            self.short_stop = ohlc4 + distance;
        }
        self.previous_held = held;

        let inside = self.previous.as_ref().is_some_and(|p| bar.high < p.high && bar.low > p.low)
            && index > 40;
        self.previous = Some(bar.clone());
        let above = bar.close > ma;
        if inside && bar.close > bar.open && above {
            strategy.entry("long", Long).size(share_of_worth(&sizing, 100.0, bar.close));
        }
        if inside && bar.close < bar.open && !above {
            strategy.entry("short", Short).size(share_of_worth(&sizing, 100.0, bar.close));
        }

        let avg = strategy.avg_cost();
        if held > 0.0 {
            let stop = self.long_stop;
            let take = avg + (avg - stop) * REWARD;
            strategy.exit("TP & SL long").from("long").take(take).stop(stop);
        } else if held < 0.0 {
            let stop = self.short_stop;
            let take = avg - (stop - avg) * REWARD;
            strategy.exit("TP & SL short").from("short").take(take).stop(stop);
        }
    }
}
