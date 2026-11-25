// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Triple EMA Trend" by cyatophilum, https://www.tradingview.com/script/vzXlqRCT/
// The original's notice: © cyatophilum
// Changed: rewritten by hand in Rust for vike.

//! A trend state: when it is idle and ADX(14) is over 43 with EMA(30) over EMA(46) over EMA(80) and the close over EMA(30),
//! it turns long, with a stop 1.8 ATR(14) under the close and a target 3.3 ATR over it; the mirror turns it short. It turns
//! idle again on the first later bar whose high crosses up through a long's target, or else whose low crosses down through its
//! stop (a short the mirror). Turning long buys 5% of the account's worth at the next open, with that stop and target as
//! its exits; turning short trades nothing. 0.075% commission; starts with 100,000.
//!
//! The state is the rule's own and never reads the position: a short turn still blocks longs until its levels are crossed,
//! and the crosses compare with the levels as they stood when last tested, so the bar after a turn tests the new level
//! against the old one. The ADX is the rule's: Wilder averages of the directional moves and of the true range, the true
//! range missing on the first bar, each line held at its last value while it has none (`Adx::with_held_lines`).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Adx;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const ADX_LEN: f64 = 14.0;
const ADX_THRESHOLD: f64 = 43.0;
const SL_ATRS: f64 = 1.8;
const TP_ATRS: f64 = 3.3;
const SHARE: f64 = 5.0;

struct S {
    fast: IndicatorLine,
    mid: IndicatorLine,
    slow: IndicatorLine,
    atr: IndicatorLine,
    adx: Adx,
    is_long: bool,
    is_short: bool,
    stop: f64,
    target: f64,
    high_tp: Cross,
    low_sl: Cross,
    low_tp: Cross,
    high_sl: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: IndicatorLine::new("ema", &[30.0]),
        mid: IndicatorLine::new("ema", &[46.0]),
        slow: IndicatorLine::new("ema", &[80.0]),
        atr: IndicatorLine::new("atr", &[14.0]),
        adx: Adx::new(ADX_LEN).expect("a valid length").with_held_lines(true),
        is_long: false,
        is_short: false,
        stop: f64::NAN,
        target: f64::NAN,
        high_tp: Cross::new(),
        low_sl: Cross::new(),
        low_tp: Cross::new(),
        high_sl: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.075), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let fast = self.fast.next(bar)[0];
        let mid = self.mid.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        let [adx, _, _] = self.adx.next(bar);
        let atr = self.atr.next(bar)[0];
        let c = bar.close;
        let long_cond = adx > ADX_THRESHOLD && fast > mid && mid > slow && c > fast;
        let short_cond = adx > ADX_THRESHOLD && fast < mid && mid < slow && c < fast;
        let idle = !(self.is_long || self.is_short);
        let entry_long = long_cond && idle;
        let entry_short = short_cond && idle;
        self.is_long |= entry_long;
        self.is_short |= entry_short;

        let cross_tp_up = matches!(self.high_tp.step(bar.high, self.target), Signal::Long);
        let cross_sl_down = matches!(self.low_sl.step(bar.low, self.stop), Signal::Short);
        let cross_tp_down = matches!(self.low_tp.step(bar.low, self.target), Signal::Short);
        let cross_sl_up = matches!(self.high_sl.step(bar.high, self.stop), Signal::Long);

        if entry_long || entry_short {
            if self.is_long {
                self.stop = c - SL_ATRS * atr;
                self.target = c + TP_ATRS * atr;
            }
            if self.is_short {
                self.stop = c + SL_ATRS * atr;
                self.target = c - TP_ATRS * atr;
            }
        } else {
            let hit = (self.is_long && (cross_tp_up || cross_sl_down))
                || (self.is_short && (cross_tp_down || cross_sl_up));
            if hit {
                self.is_long = false;
                self.is_short = false;
            }
        }

        if entry_long {
            strategy.entry("Long Entry", Long).size(share_of_worth(&sizing, SHARE, c));
            strategy.exit("Exit Long").from("Long Entry").take(self.target).stop(self.stop);
        }
    }
}
