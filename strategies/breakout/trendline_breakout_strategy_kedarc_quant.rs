// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Trendline Breakout Strategy [KedArc Quant]" by kedarcquant, https://www.tradingview.com/script/grMQIRAr/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2025-09-15, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Long 10% of the account's worth when the close crosses up through the downtrend line while it is over EMA(200); short 10% when
//! it crosses down through the uptrend line while under it. Each reverses the other. The downtrend line runs through the last
//! two pivot highs (5 bars each side) when the later is the lower, the uptrend line through the last two pivot lows when the
//! later is the higher; a line stays as it was until a newer pair forms one, and runs on to the right. Each entry states a
//! stop 2 ATR(14) from the signal close and a target 4 ATR(14) the other way, once, with the entry. 0.01% commission; starts with
//! 100,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! stacked_closes = true
//! ```

use vike_indicators::indicators::{Atr, ConfirmedPivot, Ema};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const SPAN: i64 = 5;
const STOP_ATRS: f64 = 2.0;
const TARGET_ATRS: f64 = 4.0;

/// A line through two points `(bar, price)`, read at a bar.
#[derive(Clone, Copy)]
struct Line {
    from: (i64, f64),
    to: (i64, f64),
}

impl Line {
    fn at(&self, bar: i64) -> f64 {
        let slope = (self.to.1 - self.from.1) / (self.to.0 - self.from.0) as f64;
        self.from.1 + slope * (bar - self.from.0) as f64
    }
}

struct S {
    ma: Ema,
    atr: Atr,
    pivots: ConfirmedPivot,
    bar: i64,
    /// The last two pivot highs and lows, as `(bar, price)`, the older first.
    highs: [Option<(i64, f64)>; 2],
    lows: [Option<(i64, f64)>; 2],
    down: Option<Line>,
    up: Option<Line>,
    through_down: Cross,
    through_up: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        ma: Ema::new(200.0).expect("a valid length"),
        atr: Atr::new(14.0).expect("a valid length"),
        pivots: ConfirmedPivot::new(SPAN as f64, SPAN as f64).expect("valid spans"),
        bar: -1,
        highs: [None; 2],
        lows: [None; 2],
        down: None,
        up: None,
        through_down: Cross::new(),
        through_up: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.01), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        self.bar += 1;
        let close = bar.close;
        let ma = self.ma.next(bar);
        let atr = self.atr.next(bar);
        let [high, low, _, _] = self.pivots.next(bar);
        let at = self.bar - SPAN;
        if !high.is_nan() {
            self.highs = [self.highs[1], Some((at, high))];
            if let [Some(a), Some(b)] = self.highs
                && b.1 < a.1
                && b.0 > a.0
            {
                self.down = Some(Line { from: a, to: b });
            }
        }
        if !low.is_nan() {
            self.lows = [self.lows[1], Some((at, low))];
            if let [Some(a), Some(b)] = self.lows
                && b.1 > a.1
                && b.0 > a.0
            {
                self.up = Some(Line { from: a, to: b });
            }
        }
        let down_price = self.down.map_or(f64::NAN, |l| l.at(self.bar));
        let up_price = self.up.map_or(f64::NAN, |l| l.at(self.bar));
        let crossed_above = matches!(self.through_down.step(close, down_price), Signal::Long);
        let crossed_below = matches!(self.through_up.step(close, up_price), Signal::Short);
        let long = !down_price.is_nan() && crossed_above && close > ma;
        let short = !up_price.is_nan() && crossed_below && close < ma;

        let held = strategy.position();
        // An entry the other way reverses the position, so the rule's close on an opposite signal has nothing left to close.
        if long && held <= 0.0 {
            strategy.entry("Long", Long).size(share_of_worth(&sizing, 10.0, close));
            strategy
                .exit("Long-Exit")
                .from("Long")
                .take(close + TARGET_ATRS * atr)
                .stop(close - STOP_ATRS * atr);
        }
        if short && held >= 0.0 {
            strategy.entry("Short", Short).size(share_of_worth(&sizing, 10.0, close));
            strategy
                .exit("Short-Exit")
                .from("Short")
                .take(close - TARGET_ATRS * atr)
                .stop(close + STOP_ATRS * atr);
        }
    }
}
