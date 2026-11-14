// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "The Flash-Strategy (Momentum-RSI, EMA-crossover, ATR)" by JS_TechTrading, https://www.tradingview.com/script/XKgLfo15/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2023-03-25, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! When no trade is open and RSI(10) of the 10-bar momentum is above 60: long 1,000 worth if EMA(12) is above its trailing line
//! and the supertrend (3 ATR(10) about the bar's midpoint) is up, short 1,000 worth if below and down. While the close is
//! under the supertrend a long carries a stop at it, and while over it a short does, each moved on every such bar and left
//! where it was otherwise. No costs; starts with 1,000.
//!
//! The trailing line starts 1% under the EMA and follows it from 1% away, only ever moving with it while the EMA stays on one
//! side, and jumps to the other side when the EMA crosses it. The rule's date range holds on every bar here.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Ema, Mom, Rsi, Supertrend};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::helpers::nan_to_zero;
use vike_tactics::sizing::cash_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const MOMENTUM: usize = 10;
const MOMENTUM_RSI_ABOVE: f64 = 60.0;
const FACTOR: f64 = 3.0;
const TRAIL_SHARE: f64 = 1.0 / 100.0;
const CASH: f64 = 1_000.0;

struct S {
    momentum: Mom,
    rsi: Rsi,
    supertrend: Supertrend,
    ema: Ema,
    ema_before: f64,
    trail: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        momentum: Mom::new(MOMENTUM as f64).expect("a valid length"),
        rsi: Rsi::new(MOMENTUM as f64).expect("a valid length"),
        supertrend: Supertrend::new(10.0, FACTOR).expect("valid parameters").with_down_start(true),
        ema: Ema::new(12.0).expect("a valid length"),
        ema_before: f64::NAN,
        trail: f64::NAN,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let close = bar.close;
        let momentum = self.momentum.next_value(close);
        // The RSI is fed from the first momentum on, never its missing ones.
        let mut momentum_rsi = f64::NAN;
        if !momentum.is_nan() {
            momentum_rsi = self.rsi.next(&Bar { close: momentum, ..bar.clone() });
        }
        // The rule's direction: 1 down, -1 up.
        let [line, up] = self.supertrend.next(bar);
        let dir = -up;

        let ema = self.ema.next(bar);
        let before = nan_to_zero(self.trail);
        let away = ema * TRAIL_SHARE;
        let crossed = if ema > before { ema - away } else { ema + away };
        let held_under =
            if ema < before && self.ema_before < before { before.min(ema + away) } else { crossed };
        self.trail = if ema > before && self.ema_before > before {
            before.max(ema - away)
        } else {
            held_under
        };
        self.ema_before = ema;

        if strategy.position() == 0.0 && momentum_rsi > MOMENTUM_RSI_ABOVE {
            if ema > self.trail && dir < 0.0 {
                strategy.entry("Long_0", Long).size(cash_worth(&sizing, CASH, close));
            } else if ema < self.trail && dir > 0.0 {
                strategy.entry("Short_0", Short).size(cash_worth(&sizing, CASH, close));
            }
        }
        if close < line {
            strategy.exit("Long_Close").from("Long_0").stop(line);
        }
        if close > line {
            strategy.exit("Short_Close").from("Short_0").stop(line);
        }
    }
}
