// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Ichimoku with MACD/ CMF/ TSI" by SoftKill21, https://www.tradingview.com/script/Pjsgs9JO/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2020-10-26, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Long 1 when six bulls agree, short 1 when six bears do, each reversing the other. No costs; starts with 100,000.
//!
//! The six, bull side (the bear side is the mirror): the 10-bar Ichimoku conversion line above the 30-bar base line; the
//! close above the close 25 bars back; the close above both leading lines as they stood 25 bars ago (the first is the mean of
//! conversion and base, the second the 52-bar midpoint); a MACD of the median price, SMA(12) less SMA(26), above its EMA(9);
//! the 10-bar Chaikin money flow above 0.1; the true strength index (EMA(20) of EMA(20) of the close's change, over the same
//! of its size) above 0.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use vike_indicators::strategy_kit::{max, min};
use vike_tactics::{Rule, RuleDefaults, Tactics};

use vike_indicators::indicators::{Cmf, Donchian, Ema, Sma, Tsi};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;

const OFFSET: usize = 25;

struct S {
    conversion: Donchian,
    base: Donchian,
    second: Donchian,
    /// The two leading lines, newest first, `OFFSET + 1` of them.
    leads: VecDeque<(f64, f64)>,
    /// The closes, newest first, `OFFSET + 1` of them.
    closes: VecDeque<f64>,
    fast: Sma,
    slow: Sma,
    signal: Ema,
    flow: Cmf,
    tsi: Tsi,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let donchian = |n: f64| Donchian::new(n).expect("a valid length");
    let sma = |n: f64| Sma::new(n).expect("a valid length").with_fresh_sum(true);
    let ema = |n: f64| Ema::new(n).expect("a valid length").with_skip_nan(true);
    Box::new(S {
        conversion: donchian(10.0),
        base: donchian(30.0),
        second: donchian(52.0),
        leads: VecDeque::with_capacity(OFFSET + 1),
        closes: VecDeque::with_capacity(OFFSET + 1),
        fast: sma(12.0),
        slow: sma(26.0),
        signal: ema(9.0),
        flow: Cmf::new(10.0).expect("a valid length"),
        tsi: Tsi::new(20.0, 20.0).expect("valid lengths"),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let (high, low, close) = (bar.high, bar.low, bar.close);
        let conversion = self.conversion.next(bar)[1];
        let base = self.base.next(bar)[1];
        let second = self.second.next(bar)[1];
        self.leads.push_front(((conversion + base) / 2.0, second));
        self.leads.truncate(OFFSET + 1);
        let (a, b) = self.leads.get(OFFSET).copied().unwrap_or((f64::NAN, f64::NAN));
        let (top, bottom) = (max(a, b), min(a, b));

        self.closes.push_front(close);
        self.closes.truncate(OFFSET + 1);
        let momentum = close - self.closes.get(OFFSET).copied().unwrap_or(f64::NAN);

        let median = (high + low) / 2.0;
        let macd = self.fast.next_value(median) - self.slow.next_value(median);
        let hist = macd - self.signal.next_value(macd);

        let flow = self.flow.next(bar);
        // vike's TSI counts a first change of 0 where the rule's has none; the averages forget it, and the rule reads its sign.
        let tsi = self.tsi.next(bar);

        let bull = conversion > base
            && momentum > 0.0
            && close > top
            && hist > 0.0
            && flow > 0.1
            && tsi > 0.0;
        let bear = conversion < base
            && momentum < 0.0
            && close < bottom
            && hist < 0.0
            && flow < -0.1
            && tsi < 0.0;
        if bull {
            strategy.entry("long", Long).size(1.0);
        }
        if bear {
            strategy.entry("short", Short).size(1.0);
        }
    }
}
