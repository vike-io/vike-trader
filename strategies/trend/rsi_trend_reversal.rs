// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "RSI Trend Reversal" by joseph_lemery, https://www.tradingview.com/script/hZLgJ29l/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2024-04-18, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! A threshold that trails the Heikin-Ashi close, up for a long trend and down for a short one: when the close crosses up
//! through it the trend is long and the threshold the lower band, when it crosses down the trend is short and it is the upper
//! band. Long 1 on the bar after the close crossed up, short 1 on the bar after it crossed down, each reversing the other. No
//! costs; starts with 1,000,000.
//!
//! The Heikin-Ashi close is the ticker's (`vike_indicators::strategy_kit::HeikinAshiCandles`): the bar's `(open + high + low + close) / 4` on the
//! symbol's tick, which needs no earlier candle. The bands are the rule's own and are worked out over the bars since the last
//! cross (the cross of the bar before, `BearGuy` there): the lower is the highest, over that many bars back, of the larger of
//! the highest close times `1 - (ATR / close + 1.5 / RSI)` and 0.9 of the close, each of those highs taken over the history of
//! its own expression; the upper mirrors it with the lowest, the RSI mirrored as `100 - RSI`, and 1.1 of the close. The
//! `ta.atr` is of the chart's bars, the RSI of the Heikin-Ashi close. The rule's candles are confirmed on every historical bar,
//! so its `barstate.isconfirmed` always holds.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Atr, Rsi};
use vike_indicators::strategy_kit::HeikinAshiCandles;
use vike_indicators::strategy_kit::{max, min};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::helpers::BarsSince;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const LENGTH: usize = 8;
const MULTIPLIER: f64 = 1.5;
/// `(100 - 10) / 100` and `(100 + 10) / 100`, the rule's minimum difference of 10.
const SL: f64 = 0.9;
const TP: f64 = 1.1;

/// The history of one series the rule hands a loop, newest last.
#[derive(Default)]
struct Series(Vec<f64>);

impl Series {
    /// `x[i]`, NaN before the first bar.
    fn back(&self, i: usize) -> f64 {
        self.0.len().checked_sub(i + 1).map_or(f64::NAN, |at| self.0[at])
    }

    /// The rule's `highest_custom` (`up`) or `lowest_custom` of this series, over `length + 1` bars. A NaN is never beyond
    /// the running extreme, so a NaN newest value gives NaN.
    fn extreme(&self, length: usize, up: bool) -> f64 {
        let mut x = self.back(0);
        for i in 0..=length {
            let v = self.back(i);
            if (up && v > x) || (!up && v < x) {
                x = v;
            }
        }
        x
    }
}

struct S {
    candles: HeikinAshiCandles,
    atr: Atr,
    rsi: Rsi,
    src: Series,
    /// The histories of the four expressions the loops read: each inner high and low, then each band's larger or smaller.
    high_in: Series,
    low_in: Series,
    high_mid: Series,
    low_mid: Series,
    since: BarsSince,
    bear_guy: f64,
    dir: f64,
    crossup: bool,
    crossdown: bool,
    /// The last close and threshold.
    last: (f64, f64),
    /// What the bar before signalled: `(up, down)`.
    signalled: (bool, bool),
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        candles: HeikinAshiCandles::default(),
        atr: Atr::new(LENGTH as f64).expect("a valid length"),
        rsi: Rsi::new(LENGTH as f64).expect("a valid length").with_rs_form(true),
        src: Series::default(),
        high_in: Series::default(),
        low_in: Series::default(),
        high_mid: Series::default(),
        low_mid: Series::default(),
        since: BarsSince::default(),
        bear_guy: 0.0,
        dir: f64::NAN,
        crossup: false,
        crossdown: false,
        last: (f64::NAN, f64::NAN),
        signalled: (false, false),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let src = self.candles.next(bar, strategy.tick()).close;
        self.src.0.push(src);
        let atr = self.atr.next(bar) / src;
        let rsi_lower = self.rsi.next_value(src);
        let rsi_upper = (rsi_lower - 100.0).abs();

        match self.since.step(self.crossup || self.crossdown) {
            None => self.bear_guy += 1.0,
            Some(n) => self.bear_guy = n as f64,
        }
        let n = self.bear_guy as usize;

        self.high_in
            .0
            .push(self.src.extreme(n, true) * (1.0 - (atr + (1.0 / rsi_lower * MULTIPLIER))));
        self.high_mid.0.push(max(self.high_in.extreme(n, true), src * SL));
        let lower = self.high_mid.extreme(n, true);

        self.low_in
            .0
            .push(self.src.extreme(n, false) * (1.0 + (atr + (1.0 / rsi_upper * MULTIPLIER))));
        self.low_mid.0.push(min(self.low_in.extreme(n, false), src * TP));
        let upper = self.low_mid.extreme(n, false);

        if self.dir.is_nan() {
            self.dir = 1.0;
        }
        if self.crossdown {
            self.dir = -1.0;
        }
        if self.crossup {
            self.dir = 1.0;
        }
        let thresh = if self.dir == 1.0 { lower } else { upper };

        let (last_source, last_thresh) = self.last;
        self.crossup = src > thresh && last_source <= last_thresh;
        self.crossdown = src < thresh && last_source >= last_thresh;
        self.last = (src, thresh);

        let (buy, sell) = self.signalled;
        self.signalled = (self.crossup, self.crossdown);
        if buy {
            strategy.entry("long", Long).size(1.0);
        }
        if sell {
            strategy.entry("Short", Short).size(1.0);
        }
    }
}
