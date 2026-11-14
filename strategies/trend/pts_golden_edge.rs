// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "PTS: Golden Edge" by iamjasonj, https://www.tradingview.com/script/SD5jYZuC/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2024-12-09, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Long 1 when EMA(3) of the close crosses up through EMA(33) with RSI(12) over 56, HMA(66) rising and the 14-bar high-low range
//! over half an ATR(14); short 1 on the mirror (RSI under 45, the average falling), each reversing the other. Only on the bars
//! from 2023-01-01 to 2024-12-31 23:59. No costs; starts with 1,000,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Atr, Donchian, Ema, Hma, Rsi};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::calendar::timestamp;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

struct S {
    fast: Ema,
    slow: Ema,
    hma: Hma,
    rsi: Rsi,
    atr: Atr,
    range: Donchian,
    cross: Cross,
    last_hma: f64,
    from: i64,
    to: i64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: Ema::new(3.0).expect("a valid length"),
        slow: Ema::new(33.0).expect("a valid length"),
        hma: Hma::new(66.0).expect("a valid length"),
        rsi: Rsi::new(12.0).expect("a valid length").with_rs_form(true),
        atr: Atr::new(14.0).expect("a valid length"),
        range: Donchian::new(14.0).expect("a valid length"),
        cross: Cross::new(),
        last_hma: f64::NAN,
        from: timestamp(2023, 1, 1, 0, 0),
        to: timestamp(2024, 12, 31, 23, 59),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let signal = self.cross.step(self.fast.next(bar), self.slow.next(bar));
        let hma = self.hma.next(bar);
        let rsi = self.rsi.next_value(bar.close);
        let atr = self.atr.next(bar);
        let [highest, _, lowest] = self.range.next_hl(bar.high, bar.low);
        let (rising, falling) = (hma > self.last_hma, hma < self.last_hma);
        self.last_hma = hma;
        let wide = highest - lowest > atr * 0.5;
        if bar.timestamp < self.from || bar.timestamp > self.to || !wide {
            return;
        }
        match signal {
            Signal::Long if rsi > 56.0 && rising => {
                strategy.entry("Buy", Long).size(1.0);
            }
            Signal::Short if rsi < 45.0 && falling => {
                strategy.entry("Sell", Short).size(1.0);
            }
            _ => {}
        }
    }
}
