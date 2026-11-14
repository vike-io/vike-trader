// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from the script by PineIndicators, https://www.tradingview.com/script/dagKHrx4/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2025-03-13, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Three falling bars then three rising ones, the last close over the first falling bar's open and the last falling close under
//! the first falling one's, make a support level at the first rising bar's low; the mirror (three rising, then three falling)
//! makes a resistance level at the first falling bar's high. From the 15th bar on, a close crossing up through the latest
//! resistance goes long all of the account, a close crossing down through the latest support goes short, each once per level,
//! reversing the other side; orders fill at the signal bar's close. From the bar after, while held, a stop one ATR(14) and a
//! target six ATR(14) from the last signal's close on that side, with the ATR of that bar; a later signal on the side held moves
//! them. 0.01% commission, 2 ticks of slippage; starts with 100,000.
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
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;

const UP: &str = "L";
const DOWN: &str = "S";
const LEN: usize = 3;
const FIRST_BAR: usize = 14;
const RISK_REWARD: f64 = 6.0;

struct S {
    atr: IndicatorLine,
    /// The last `2 * LEN` bars, newest first.
    bars: VecDeque<Bar>,
    seen: usize,
    resistance: f64,
    support: f64,
    previous_close: f64,
    previous_resistance: f64,
    previous_support: f64,
    long_level: f64,
    short_level: f64,
    long_from: (f64, f64),
    short_from: (f64, f64),
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        atr: IndicatorLine::new("atr", &[14.0]),
        bars: VecDeque::with_capacity(2 * LEN + 1),
        seen: 0,
        resistance: f64::NAN,
        support: f64::NAN,
        previous_close: f64::NAN,
        previous_resistance: f64::NAN,
        previous_support: f64::NAN,
        long_level: f64::NAN,
        short_level: f64::NAN,
        long_from: (f64::NAN, f64::NAN),
        short_from: (f64::NAN, f64::NAN),
    })
}

impl S {
    /// `rising` for the newest `LEN` bars and the opposite for the `LEN` before them, as the rule reads green and red.
    fn run(&self, newest_rise: bool) -> bool {
        let rises = |b: &Bar| if newest_rise { b.close > b.open } else { b.close < b.open };
        let falls = |b: &Bar| if newest_rise { b.close < b.open } else { b.close > b.open };
        self.bars.len() == 2 * LEN
            && (0..LEN).all(|i| rises(&self.bars[i]) && falls(&self.bars[LEN + i]))
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.01), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let atr = self.atr.next(bar)[0];
        self.bars.push_front(bar.clone());
        self.bars.truncate(2 * LEN);
        let index = self.seen;
        self.seen += 1;
        let (previous_close, previous_resistance, previous_support) =
            (self.previous_close, self.previous_resistance, self.previous_support);
        self.previous_close = bar.close;
        if index >= FIRST_BAR && !atr.is_nan() {
            let oldest = &self.bars[2 * LEN - 1];
            let third = &self.bars[LEN];
            if self.run(true) && bar.close > oldest.open && third.close < oldest.close {
                self.support = self.bars[LEN - 1].low;
            }
            if self.run(false) && bar.close < oldest.open && third.close > oldest.close {
                self.resistance = self.bars[LEN - 1].high;
            }

            let up = self.resistance;
            if bar.close > up && previous_close <= previous_resistance && self.long_level != up {
                let size = share_of_worth(&sizing, 100.0, sizing.slipped(Long, bar.close));
                strategy.entry(UP, Long).size(size);
                self.long_level = up;
                self.long_from = (bar.close, atr);
            }
            let down = self.support;
            if bar.close < down && previous_close >= previous_support && self.short_level != down {
                let size = share_of_worth(&sizing, 100.0, sizing.slipped(Short, bar.close));
                strategy.entry(DOWN, Short).size(size);
                self.short_level = down;
                self.short_from = (bar.close, atr);
            }

            let held = strategy.position();
            if held > 0.0 && !self.long_from.0.is_nan() {
                let (price, atr) = self.long_from;
                strategy.exit("L Exit").from(UP).take(price + atr * RISK_REWARD).stop(price - atr);
            }
            if held < 0.0 && !self.short_from.0.is_nan() {
                let (price, atr) = self.short_from;
                strategy
                    .exit("S Exit")
                    .from(DOWN)
                    .take(price - atr * RISK_REWARD)
                    .stop(price + atr);
            }
        }
        self.previous_resistance = self.resistance;
        self.previous_support = self.support;
    }
}
