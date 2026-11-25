// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "QuantNomad - Heikin-Ashi PSAR Strategy" by QuantNomad, https://www.tradingview.com/script/0iKy7lyG/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2020-08-13, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! A parabolic SAR (start 0.02, step 0.02, at most 0.2) run on Heikin-Ashi candles. Long 1 when a falling SAR flips because
//! the Heikin-Ashi close reaches the last SAR, short 1 when a rising one flips the other way, each reversing the other. No
//! costs; starts with 100,000.
//!
//! The SAR is vike's `Psar` judged on the close (`Psar::with_reversal_on_close`) fed the candles: it starts from the last
//! candle's colour, a flip puts the SAR at the last extreme, and each other bar moves it by the factor toward the extreme
//! updated on this bar. The rule starts it twice, on its second and third bars, each time from the candle before. It extends a
//! falling trend's factor on a new low of the plain bar, not of the candle; the two are the same test, because a candle's
//! open, the midpoint of the last candle's body, is never under a falling trend's extreme. The rule's date range (2018 to
//! 2100) holds on every bar.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::Indicator;
use vike_indicators::indicators::{HeikinAshi, Psar};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const START: f64 = 0.02;
const STEP: f64 = 0.02;
const MOST: f64 = 0.2;

struct S {
    candles: HeikinAshi,
    sar: Psar,
    /// The last bar's Heikin-Ashi candle.
    last: Option<Bar>,
    /// The SAR and its direction the last bar left.
    value: f64,
    rising: Option<bool>,
    bars: usize,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let sar = Psar::new(STEP, MOST).and_then(|p| p.with_start(START)).expect("valid factors");
    Box::new(S {
        candles: HeikinAshi::default(),
        sar: sar.with_reversal_on_close(true),
        last: None,
        value: f64::NAN,
        rising: None,
        bars: 0,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let [open, high, low, ha_close] = self.candles.next(bar);
        let candle = Bar { open, high, low, close: ha_close, ..bar.clone() };
        let to_short = self.rising == Some(true) && ha_close <= self.value;
        let to_long = self.rising == Some(false) && ha_close >= self.value;
        if self.bars == 2 {
            // The rule's second start: from the last candle again.
            self.sar.reset();
            self.sar.next(self.last.as_ref().expect("two bars are in"));
        }
        self.value = self.sar.next(&candle);
        self.rising = self.sar.uptrend();
        self.last = Some(candle);
        self.bars += 1;

        if to_long {
            strategy.entry("long", Long).size(1.0);
        }
        if to_short {
            strategy.entry("short", Short).size(1.0);
        }
    }
}
