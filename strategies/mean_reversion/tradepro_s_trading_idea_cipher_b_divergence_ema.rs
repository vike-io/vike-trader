// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "TradePro's Trading Idea Cipher B+ Divergence EMA Pullback Strategy" by TradingStrategyCheck, https://www.tradingview.com/script/ywcSiaaV/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2021-07-05, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Long all of the account's worth, short on the mirror, when the wave trend crosses in its extreme zone with the close on the
//! right side of a 150-bar EMA, a money-flow reading agrees, and the close has just crossed a 50-bar EMA after three
//! closes on the other side of it somewhere in the last ten bars. One position at a time, never reversed. Once held, a
//! target 2 ATR(11) and a stop 4 ATR(11) from the cost, both worked out on the bar the position first shows and kept. No
//! costs; starts with 5,000, kept in USD.
//!
//! The wave trend is EMA(10) of `(ap - EMA(ap, 14)) / (0.015 * EMA(|ap - EMA(ap, 14)|, 14))`, `ap` the typical price, and its
//! signal line SMA(4) of that. Long wants the signal line at or under -60 with the wave trend crossing it and at or over it,
//! and under 0; short wants it at or over 53 with the wave trend at or under it, and over 0. The money-flow reading is
//! SMA(71) of `200 * (close - open) / (high - low)` less 1.5. The higher-timeframe calls ask for the chart's own timeframe,
//! so each is its expression on the chart.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use vike_tactics::rolling::CrossEither;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

use vike_indicators::indicators::{Atr, Ema, Sma, Wavetrend};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;

const LENGTH: usize = 11;

struct S {
    wave: Wavetrend,
    cross: CrossEither,
    flow: Sma,
    slow: Ema,
    fast: Ema,
    atr: Atr,
    /// `(close, fast EMA)` newest first, `LENGTH` of them.
    seen: VecDeque<(f64, f64)>,
    /// `(stop, target)` of the position held, from the bar it first showed.
    levels: Option<(f64, f64)>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let ema = |n: f64| Ema::new(n).expect("a valid length").with_skip_nan(true);
    Box::new(S {
        wave: Wavetrend::new(14.0, 10.0).expect("valid lengths"),
        cross: CrossEither::default(),
        flow: Sma::new(71.0).expect("a valid length"),
        slow: ema(150.0),
        fast: ema(50.0),
        atr: Atr::new(11.0).expect("a valid length"),
        seen: VecDeque::with_capacity(LENGTH),
        levels: None,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let close = bar.close;
        let [wt1, wt2] = self.wave.next(bar);
        let crossed = self.cross.step(wt1, wt2);
        let buy = crossed && wt2 <= -60.0 && wt2 - wt1 <= 0.0;
        let sell = crossed && wt2 >= 53.0 && wt2 - wt1 >= 0.0;
        let flow = self.flow.next_value((close - bar.open) / (bar.high - bar.low) * 200.0) - 1.5;
        let slow = self.slow.next_value(close);
        let fast = self.fast.next_value(close);
        let atr = self.atr.next(bar);

        // `seen[i]` is the bar `i + 1` back: the closes and the fast EMA the pullback tests read.
        let at = |i: usize| self.seen.get(i - 1).copied().unwrap_or((f64::NAN, f64::NAN));
        let run = |from: usize, above: bool| {
            (from..from + 3).all(|i| {
                let (c, e) = at(i);
                if above { c > e } else { c < e }
            })
        };
        let after = |above: bool| [2, 5, 8].iter().any(|&from| run(from, above));
        let (c1, e1) = at(1);
        let long = close > slow
            && wt2 < 0.0
            && buy
            && flow > 0.0
            && close > fast
            && c1 < e1
            && after(true);
        let short = close < slow
            && wt2 > 0.0
            && sell
            && flow < 0.0
            && close < fast
            && c1 > e1
            && after(false);
        self.seen.push_front((close, fast));
        self.seen.truncate(LENGTH);

        let held = strategy.position();
        if held == 0.0 {
            self.levels = None;
        } else if self.levels.is_none() {
            let cost = strategy.avg_cost();
            let side = held.signum();
            self.levels = Some((cost - side * atr * 4.0, cost + side * atr * 2.0));
        }
        if held == 0.0 && long {
            strategy.entry("long", Long).size(share_of_worth(&sizing, 100.0, close));
        }
        if held == 0.0 && short {
            strategy.entry("short", Short).size(share_of_worth(&sizing, 100.0, close));
        }
        if let Some((cut, take)) = self.levels {
            let name = if held > 0.0 { "long" } else { "short" };
            strategy.exit("exit").from(name).take(take).stop(cut);
        }
    }
}
