// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "FS ATR & PS (MA)" by ahmad_naquib, https://www.tradingview.com/script/OZnjwvZv/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2020-07-03, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! When flat, long when EMA(10) of the close crosses up through WMA(20), short when it crosses down. The size risks a tenth of
//! the account's worth over 200 ticks, `floor(worth * 0.1 / (200 * tick))`. With the entry go two exits, each with a stop
//! 200 ticks from the signal close against the position: half the size at a target 200 ticks with it, the rest at a target
//! 400 ticks with it. No costs; starts with 100. **Does not reproduce its reference** (1229 trades, net -92.767136 on
//! CAPITALCOM:EURUSD 30m, 2025-01-01..2026-08-25): checked on Dukascopy's EURUSD, another feed's prices, this file
//! gives 1229 / -91.430255 on bars of the mid price, which it is run on (the nearer count), and 1221 / -94.566015 on
//! bars of the bid; the owner accepted those numbers on 2026-11-16.
//!
//! The original's time window (from 2019-01-01, no end) holds on every bar it runs on; the plot is left out.
//!
//! **Data:** the reference ran on CAPITALCOM:EURUSD, a CFD feed; this file is checked on Dukascopy's EURUSD, a forex feed.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Ema, Wma};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

/// The stop and the first target, and the second target, in ticks from the signal close.
const NEAR_TICKS: f64 = 200.0;
const FAR_TICKS: f64 = 400.0;
const RISK: f64 = 0.1;

struct S {
    ema: Ema,
    wma: Wma,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        ema: Ema::new(10.0).expect("a valid length"),
        wma: Wma::new(20.0).expect("a valid length"),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let signal = self.cross.step(self.ema.next(bar), self.wma.next(bar));
        if strategy.position() != 0.0 {
            return;
        }
        let near = NEAR_TICKS * tick;
        let far = FAR_TICKS * tick;
        let size = (strategy.equity() * RISK / near).floor();
        let close = bar.close;
        match signal {
            Signal::Long => {
                strategy.entry("Buy", Long).size(size);
                strategy
                    .exit("Exit1")
                    .from("Buy")
                    .size(size / 2.0)
                    .stop(close - near)
                    .take(close + near);
                strategy.exit("Exit2").from("Buy").stop(close - near).take(close + far);
            }
            Signal::Short => {
                strategy.entry("Sell", Short).size(size);
                strategy
                    .exit("Exit3")
                    .from("Sell")
                    .size(size / 2.0)
                    .stop(close + near)
                    .take(close - near);
                strategy.exit("Exit4").from("Sell").stop(close + near).take(close - far);
            }
            _ => {}
        }
    }
}
