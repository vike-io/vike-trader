// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Gold Intraday EMA/BB/VWAP + ATR SL/TP + Session/Weekday Filter" by suri14373, https://www.tradingview.com/script/mZYK8jsg/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2026-08-05, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Long a tenth of the account's worth when EMA(9) crosses up through SMA(20) with the close above EMA(21) and the day's VWAP of
//! the close, short it on the mirror, each reversing the other; only on bars opening Monday to Thursday before 16:00 UTC. Each
//! entry carries, from its signal bar, a stop 1.5 ATR(14) from that close and a target 2 ATR from it. A signal on the side
//! already held adds nothing and moves that exit. No costs; starts with 100,000, version 6's default.
//!
//! On a day's first bar the VWAP is that close exactly (`Vwap::with_first_bar_price`), where `price * volume / volume` can
//! land an ulp off it, and a signal at 00:00 can turn on that ulp.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Vwap, VwapSource};
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::calendar::clock;
use vike_tactics::helpers::weekday;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const SHARE: f64 = 10.0;
const STOP_ATRS: f64 = 1.5;
const TARGET_ATRS: f64 = 2.0;
/// Trading stops at this UTC hour: the Asian and London sessions are on, New York off.
const LAST_HOUR: i64 = 16;

struct S {
    fast: IndicatorLine,
    slow: IndicatorLine,
    mid: IndicatorLine,
    atr: IndicatorLine,
    vwap: Vwap,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: IndicatorLine::new("ema", &[9.0]),
        slow: IndicatorLine::new("ema", &[21.0]),
        mid: IndicatorLine::new("sma", &[20.0]),
        atr: IndicatorLine::new("atr", &[14.0]),
        vwap: Vwap::default().with_source(VwapSource::Close).with_first_bar_price(true),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        let mid = self.mid.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        let vwap = self.vwap.next(bar);
        let cross = self.cross.step(fast, mid);
        // Monday to Thursday.
        if clock(bar).0 >= LAST_HOUR || weekday(bar.timestamp) > 3 {
            return;
        }
        let (side, entry, exit) = match cross {
            Signal::Long if bar.close > slow && bar.close > vwap => (Long, "BUY", "BUY Exit"),
            Signal::Short if bar.close < slow && bar.close < vwap => (Short, "SELL", "SELL Exit"),
            _ => return,
        };
        let way = f64::from(side);
        strategy.entry(entry, side).size(share_of_worth(&sizing, SHARE, bar.close));
        strategy
            .exit(exit)
            .from(entry)
            .take(bar.close + way * TARGET_ATRS * atr)
            .stop(bar.close - way * STOP_ATRS * atr);
    }
}
