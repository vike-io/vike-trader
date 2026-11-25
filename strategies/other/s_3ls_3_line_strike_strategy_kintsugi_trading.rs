// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "3LS | 3 Line Strike Strategy [Kintsugi Trading]" by KintsugiTrading, https://www.tradingview.com/script/fJEKAdgD/
// The original's notice: © KintsugiTrading
// Changed: rewritten by hand in Rust for vike.

//! Three candles of one colour followed by an engulfing candle of the other (a body larger than the one before it), taken with
//! the trend and volume: long on three red candles and a bullish engulfing when volume is over 1.5 times its 30-bar average and
//! SMA(21) is over SMA(55); short on the mirror. One trade at a time, filling at the signal close, sized to risk 2% of the
//! account's worth to the stop (worth times 0.02 over the stop's distance, rounded to 6 decimals, then to the step). The stop is
//! 0.3 past the signal bar's low (high) and the target is as far from the close as the stop is. The exit is placed with the
//! entry. No costs; starts with 10,000.
//!
//! The rule's other stop types, session filter and alert strings are off or text only; its call for another symbol's daily close
//! feeds those strings alone and is not ported.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! fill = "close"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use vike_tactics::helpers::{back, push};
use vike_tactics::{Rule, RuleDefaults, Tactics};

use vike_indicators::indicators::Sma;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;

/// The rule's "additional stop size" in pips, a pip being 10 ticks of the instrument.
const STOP_PAD_PIPS: f64 = 3.0;
const PIP_TICKS: f64 = 10.0;
const RISK_SHARE: f64 = 0.02;

/// `1` for a green candle, `-1` for a red one, `0` for a doji.
fn colour(open: f64, close: f64) -> i32 {
    (close > open) as i32 - (close < open) as i32
}

struct S {
    opens: VecDeque<f64>,
    closes: VecDeque<f64>,
    volume_average: Sma,
    fast: Sma,
    slow: Sma,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let sma = |n: f64| Sma::new(n).expect("a valid length");
    Box::new(S {
        opens: VecDeque::new(),
        closes: VecDeque::new(),
        volume_average: sma(30.0),
        fast: sma(21.0),
        slow: sma(55.0),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let stop_pad = STOP_PAD_PIPS * tick * PIP_TICKS;
        let sizing = strategy.sizing();
        push(&mut self.opens, bar.open, 4);
        push(&mut self.closes, bar.close, 4);
        let volume_ok = bar.volume > self.volume_average.next_value(bar.volume) * 1.5;
        let (fast, slow) = (self.fast.next_value(bar.close), self.slow.next_value(bar.close));
        if self.opens.len() < 4 {
            return;
        }
        let colours: Vec<i32> =
            (0..4).map(|i| colour(back(&self.opens, i), back(&self.closes, i))).collect();
        let body = |i: usize| (back(&self.closes, i) - back(&self.opens, i)).abs();
        let engulfs = body(0) > body(1);
        let bull = colours[1..].iter().all(|&c| c < 0) && colours[0] > 0 && engulfs;
        let bear = colours[1..].iter().all(|&c| c > 0) && colours[0] < 0 && engulfs;
        if strategy.position() != 0.0 || !volume_ok {
            return;
        }
        let close = bar.close;
        let worth = strategy.equity();
        if bull && fast > slow {
            let stop = bar.low - stop_pad;
            let risk = close - stop;
            let size = sizing.floor(((worth * RISK_SHARE / risk) * 1e6).round() / 1e6);
            strategy.entry("Long", Long).size(size);
            strategy.exit("Long Exit").from("Long").take(close + risk).stop(stop);
        }
        if bear && fast < slow {
            let stop = bar.high + stop_pad;
            let risk = stop - close;
            let size = sizing.floor(((worth * RISK_SHARE / risk) * 1e6).round() / 1e6);
            strategy.entry("Short", Short).size(size);
            strategy.exit("Short Exit").from("Short").take(close - risk).stop(stop);
        }
    }
}
