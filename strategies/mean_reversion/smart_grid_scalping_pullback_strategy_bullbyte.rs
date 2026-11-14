// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Smart Grid Scalping(Pullback) Strategy[BullByte]" by BullByte, https://www.tradingview.com/script/B3Ey5I4g/
// The original's notice: © BullByte
// Changed: rewritten by hand in Rust for vike.

//! A grid around the close 20 bars back, spaced 0.35 ATR(10), set from the 22nd bar on. Short 30% of the account's worth at
//! the next open on a down bar closing above the sixth line up, with a range over 0.3% of its high and RSI(14) over 70; long
//! on an up bar closing under the sixth line down, with a range over 0.3% of its low and RSI under 30. Each reverses the
//! other. Every bar a long is held its exit is stated afresh from this close: a target 0.4% over it, and a trailing stop
//! that arms at 0.2% over it and then follows the best price by ATR(10) read as a count of ticks; a short is the mirror.
//! 0.04% commission, 1 tick of slippage; starts with 100,000.
//!
//! A restated exit whose trailing levels moved is a new order, so each bar's trailing stop starts unarmed. The reference
//! cuts a fractional tick count of the trail to whole ticks and rounds the arming level toward the position.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics, TacticsTrail};

use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;

const BACK: usize = 20;
const GRID: f64 = 0.35;
const LEVEL: f64 = 5.0;
const PROFIT: f64 = 0.004;
const NO_TRADE: f64 = 0.003;
const SHARE: f64 = 30.0;

struct S {
    atr: IndicatorLine,
    rsi: IndicatorLine,
    closes: VecDeque<f64>,
    seen: usize,
    up: f64,
    down: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        atr: IndicatorLine::new("atr", &[10.0]),
        rsi: IndicatorLine::new("rsi", &[14.0]),
        closes: VecDeque::with_capacity(BACK + 2),
        seen: 0,
        up: f64::NAN,
        down: f64::NAN,
    })
}

/// The rule's rounding of a level to `tick`, down (`side < 0`) or up, a level within 1e-4 of a tick counting as on it.
fn tick_round(price: f64, side: i32, tick: f64) -> f64 {
    let ticks = price / tick;
    if side < 0 { (ticks + 1e-4).floor() * tick } else { (ticks - 1e-4).ceil() * tick }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.04), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let sizing = strategy.sizing();
        let atr = self.atr.next(bar)[0];
        let rsi = self.rsi.next(bar)[0];
        self.closes.push_front(bar.close);
        self.closes.truncate(BACK + 1);
        if self.seen > BACK {
            let base = self.closes[BACK];
            self.up = base + (LEVEL + 1.0) * atr * GRID;
            self.down = base - (LEVEL + 1.0) * atr * GRID;
        }
        self.seen += 1;
        let (o, h, l, c) = (bar.open, bar.high, bar.low, bar.close);
        if c > self.up && (h - l) / h > NO_TRADE && rsi > 70.0 && c < o {
            strategy.entry("Short", Short).size(share_of_worth(&sizing, SHARE, c));
        }
        if c < self.down && (h - l) / l > NO_TRADE && rsi < 30.0 && c > o {
            strategy.entry("Long", Long).size(share_of_worth(&sizing, SHARE, c));
        }
        let held = strategy.position();
        let Some(entry) = strategy.lots().first().map(|lot| lot.price) else { return };
        let by_ticks = atr.trunc();
        if held > 0.0 {
            let arm = tick_round(c * (1.0 + PROFIT * 0.5), -1, tick);
            let trail = TacticsTrail { after_ticks: ((arm - entry) / tick).round(), by_ticks };
            strategy.exit("Exit Long").from("Long").take(c * (1.0 + PROFIT)).trail(trail);
        }
        if held < 0.0 {
            let arm = tick_round(c * (1.0 - PROFIT * 0.5), 1, tick);
            let trail = TacticsTrail { after_ticks: ((entry - arm) / tick).round(), by_ticks };
            strategy.exit("Exit Short").from("Short").take(c * (1.0 - PROFIT)).trail(trail);
        }
    }
}
