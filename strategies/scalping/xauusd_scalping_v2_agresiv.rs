// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "XAUUSD Scalping V2 AGRESIV" by Andra2525, https://www.tradingview.com/script/5q4PsY3a/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2026-05-03, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Long a tenth of the account's worth when EMA(5) crosses up through EMA(13) with RSI(7) over 55, short it when EMA(5) crosses
//! down with RSI(7) under 45, each reversing the other; ATR(14) must be over 0.5. Each entry carries, from its signal bar, a
//! target 1.2 ATR from that close, a stop 0.8 ATR from it, and an exit `0.5 * ATR` past the entry, read as a count of ticks
//! with no offset. A signal on the side already held adds nothing and moves that exit. No costs; starts with 1,000,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics, TacticsTrail};

const SHARE: f64 = 10.0;
const TARGET_ATRS: f64 = 1.2;
const STOP_ATRS: f64 = 0.8;
const TRAIL_ATRS: f64 = 0.5;
const MIN_ATR: f64 = 0.5;

struct S {
    fast: IndicatorLine,
    slow: IndicatorLine,
    rsi: IndicatorLine,
    atr: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: IndicatorLine::new("ema", &[5.0]),
        slow: IndicatorLine::new("ema", &[13.0]),
        rsi: IndicatorLine::new("rsi", &[7.0]),
        atr: IndicatorLine::new("atr", &[14.0]),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let sizing = strategy.sizing();
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        let rsi = self.rsi.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        let cross = self.cross.step(fast, slow);
        if atr.is_nan() || atr <= MIN_ATR {
            return;
        }
        let (side, entry, exit) = match cross {
            Signal::Long if rsi > 55.0 => (Long, "BUY", "TP/SL BUY"),
            Signal::Short if rsi < 45.0 => (Short, "SELL", "TP/SL SELL"),
            _ => return,
        };
        let way = f64::from(side);
        strategy.entry(entry, side).size(share_of_worth(&sizing, SHARE, bar.close));
        strategy
            .exit(exit)
            .from(entry)
            .take(bar.close + way * atr * TARGET_ATRS)
            .stop(bar.close - way * atr * STOP_ATRS)
            .trail(TacticsTrail { after_ticks: (atr * TRAIL_ATRS / tick).ceil(), by_ticks: 0.0 });
    }
}
