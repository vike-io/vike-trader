// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "BuyTheDip" by racer8, https://www.tradingview.com/script/AHxkEMXm/
// The original's notice: © racer8
// Changed: rewritten by hand in Rust for vike.

//! Long 1 when the close falls under the lowest low of the 7 bars before this one. On the bar the position grows, a target
//! twice the ATR over the average price and a stop twice the ATR under it are fixed, the ATR being SMA(10) of the true range
//! as of the bar before; while long, an exit rests at both. One entry at a time; no costs; starts with 100,000.
//!
//! The levels are fixed only on a bar the position grows, so an entry filled before the ATR has its first value fixes none,
//! and nothing ever exits it. That is what happens over the reference's window: the first entry fills on its 11th bar, one
//! bar before the ATR's first value, and no trade closes, as in the reference.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;

use vike_indicators::indicators::Sma;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "Buy";
/// The bars whose lowest low the close must fall under, before this one.
const DIP: usize = 7;
const ATR_TIMES: f64 = 2.0;

struct S {
    average_range: Sma,
    /// The ATR as of the bar before.
    last_range: f64,
    last_close: f64,
    /// The lows of the last `DIP` bars, the newest last.
    lows: VecDeque<f64>,
    /// The position at the last bar's close.
    held: f64,
    /// The stop and the target fixed on the last bar the position grew.
    fixed: (f64, f64),
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        average_range: Sma::new(10.0).expect("a valid length"),
        last_range: f64::NAN,
        last_close: f64::NAN,
        lows: VecDeque::with_capacity(DIP + 1),
        held: 0.0,
        fixed: (f64::NAN, f64::NAN),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        // The true range has no value on the first bar, which has no close before it.
        let range = (bar.high - bar.low)
            .max((bar.high - self.last_close).abs())
            .max((bar.low - self.last_close).abs());
        let range = if self.last_close.is_nan() { f64::NAN } else { range };
        let atr = self.last_range;
        self.last_range =
            if range.is_nan() { f64::NAN } else { self.average_range.next_value(range) };
        self.last_close = bar.close;

        let held = strategy.position();
        if held > self.held {
            let cost = strategy.avg_cost();
            self.fixed = (cost - ATR_TIMES * atr, cost + ATR_TIMES * atr);
        }
        self.held = held;

        let dip = if self.lows.len() == DIP {
            self.lows.iter().copied().fold(f64::INFINITY, f64::min)
        } else {
            f64::NAN
        };
        self.lows.push_back(bar.low);
        if self.lows.len() > DIP {
            self.lows.pop_front();
        }

        if bar.close < dip {
            strategy.entry(NAME, Long);
        }
        let (stop, take) = self.fixed;
        if held > 0.0 && !stop.is_nan() && !take.is_nan() {
            strategy.exit("Stop").from(NAME).stop(stop).take(take);
        }
    }
}
