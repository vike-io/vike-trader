// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Zero-Lag MA Trend Following" by PakunFX, https://www.tradingview.com/script/7kbjZ4he/
// The original's notice: © PakunFX
// Changed: rewritten by hand in Rust for vike.

//! A zero-lag average, EMA(34) of `2 * close - EMA(34)`, against EMA(34). When it crosses up, a box from it down by ATR(14)
//! is drawn and a long is armed; when it crosses down, a box from it up by ATR(14) and a short is armed. Each arm waits
//! independently. An armed long enters 10% of the account's worth when the low crosses up through the latest box's top, with
//! a stop at its bottom and a target twice that distance above the close; an armed short enters when the high crosses down
//! through the latest box's bottom, the mirror. Entering disarms that side; the opposite entry reverses. No costs; starts with
//! 1,000,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Ema;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const LENGTH: f64 = 34.0;
const REWARD: f64 = 2.0;
const SHARE: f64 = 10.0;

struct S {
    ema: Ema,
    zero_lag: Ema,
    atr: IndicatorLine,
    turn: Cross,
    /// `(top, bottom)` of the latest box.
    zone: Option<(f64, f64)>,
    long_armed: bool,
    short_armed: bool,
    rise: Cross,
    fall: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        ema: Ema::new(LENGTH).expect("a valid length"),
        zero_lag: Ema::new(LENGTH).expect("a valid length").with_skip_nan(true),
        atr: IndicatorLine::new("atr", &[14.0]),
        turn: Cross::new(),
        zone: None,
        long_armed: false,
        short_armed: false,
        rise: Cross::new(),
        fall: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let atr = self.atr.next(bar)[0];
        let ema = self.ema.next_value(bar.close);
        // Left: the EMA of `2 * close - EMA` seeds with the mean of those differences, which no `Dema` start gives.
        let zero_lag = self.zero_lag.next_value(bar.close + (bar.close - ema));
        match self.turn.step(zero_lag, ema) {
            Signal::Long => {
                self.zone = Some((zero_lag, zero_lag - atr));
                self.long_armed = true;
            }
            Signal::Short => {
                self.zone = Some((zero_lag + atr, zero_lag));
                self.short_armed = true;
            }
            _ => {}
        }
        let (top, bottom) = self.zone.unwrap_or((f64::NAN, f64::NAN));
        let rise = matches!(self.rise.step(bar.low, top), Signal::Long);
        let fall = matches!(self.fall.step(bar.high, bottom), Signal::Short);
        if self.long_armed && rise {
            let take = bar.close + (bar.close - bottom) * REWARD;
            strategy.entry("long", Long).size(share_of_worth(&sizing, SHARE, bar.close));
            strategy.exit("long exit").from("long").take(take).stop(bottom);
            self.long_armed = false;
        }
        if self.short_armed && fall {
            let take = bar.close - (top - bar.close) * REWARD;
            strategy.entry("short", Short).size(share_of_worth(&sizing, SHARE, bar.close));
            strategy.exit("short exit").from("short").take(take).stop(top);
            self.short_armed = false;
        }
    }
}
