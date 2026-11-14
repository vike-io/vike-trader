// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Moving Average Displaced Envelope & ATRTS" by HPotter, https://www.tradingview.com/script/n45eXKBG/
// The original's notice: Copyright by HPotter v1.0 27/09/2022
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2022-09-27, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! An envelope 0.5% either side of the 9-bar EMA of the close 13 bars back: the side is up from a close over its top until a
//! close under its bottom, and down from that until a close over the top. An ATR trailing stop runs beside it: while the
//! close and the close before stay above the stop line it rises to `close - 2 * ATR(15)` and never falls, the mirror while
//! both stay below, and a close through it restarts it on the other side. On the up side with the close above that stop,
//! long 1 (reversing a short); with the close not above it, a long is closed. The down side is the mirror. No costs; starts
//! with 1,000,000.
//!
//! The stop starts from 0 (the rule's `nz`) once the ATR has a value, which makes its first value `close - 2 * ATR`: vike's
//! `AtrTrailingStop`.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{AtrTrailingStop, Ema};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const DISPLACEMENT: usize = 13;

struct S {
    average: Ema,
    /// The average on the last `DISPLACEMENT + 1` bars, newest first.
    past: [f64; DISPLACEMENT + 1],
    stop: AtrTrailingStop,
    /// `1` up, `-1` down, `0` before the envelope was first left.
    side: i32,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        average: Ema::new(9.0).expect("a valid length"),
        past: [f64::NAN; DISPLACEMENT + 1],
        stop: AtrTrailingStop::new(15.0, 2.0).expect("valid parameters"),
        side: 0,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        self.past.rotate_right(1);
        self.past[0] = self.average.next(bar);
        let top = self.past[DISPLACEMENT] * ((100.0 + 0.5) / 100.0);
        let bottom = self.past[DISPLACEMENT] * ((100.0 - 0.5) / 100.0);

        let close = bar.close;
        let stop = self.stop.next(bar);
        let (above, below) = (close > stop, close < stop);

        if close < bottom {
            self.side = -1;
        } else if close > top {
            self.side = 1;
        }
        let held = strategy.position();
        match self.side {
            1 if above => {
                strategy.entry("long", Long).size(1.0);
            }
            1 if held > 0.0 => {
                strategy.close_all();
            }
            -1 if below => {
                strategy.entry("short", Short).size(1.0);
            }
            -1 if held < 0.0 => {
                strategy.close_all();
            }
            _ => {}
        }
    }
}
