// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "MTF Layered RSI - Bitcoin Bot [wbburgin]" by wbburgin, https://www.tradingview.com/script/z1fsrtce/
// The original's notice: © wbburgin
// Changed: rewritten by hand in Rust for vike.

//! Three RSIs of the close, long only: the RSI(7) of the chart's own bars, and two layered ones the original works out on
//! the chart's bars in place of a 3-minute and a 5-minute timeframe ("manual calculation", its default): the change of the
//! close over 3 (and 5) bars, its rises and its falls each smoothed by Wilder's average over 7 times that many bars (21 and
//! 35), and the RSI of the two. A long of one unit when the chart's RSI crosses up through 20 while both layered ones are
//! under 20, or when one layered RSI crosses up through 20 while the other and the chart's are over 20; up to two longs
//! are held. Every long is closed when the chart's RSI crosses down through 80. No costs; starts with 10,000, no margin
//! stated.
//!
//! The original is written for 1-minute bars: its layers are 3 and 5 chart bars because 3 and 5 minutes are that many
//! 1-minute bars, so they are written as 3 and 5 here. Its timeframe calls feed only its other calculation mode, which the
//! default does not read, and its short side, EMA filter and percent stop are off by default: none of them is ported. The
//! plots are left out.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;

use vike_indicators::indicators::{Rsi, Smma};
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::helpers::{back, push};
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const LENGTH: f64 = 7.0;
const OVERSOLD: f64 = 20.0;
const OVERBOUGHT: f64 = 80.0;
/// The two layers, in chart bars: 3 and 5 minutes of the original's 1-minute bars.
const FIRST_LAYER: usize = 3;
const SECOND_LAYER: usize = 5;

/// One layered RSI: the change of the close over `bars` bars, its rises and falls each Wilder-smoothed over `LENGTH * bars`.
struct Layer {
    bars: usize,
    rises: Smma,
    falls: Smma,
}

impl Layer {
    fn new(bars: usize) -> Self {
        let average = || {
            Smma::new(LENGTH * bars as f64)
                .expect("a valid length")
                .with_skip_nan(true)
                .with_alpha_steps(true)
        };
        Layer { bars, rises: average(), falls: average() }
    }

    /// The RSI as of the newest close in `closes` (kept newest first); NaN until both averages are seeded.
    fn next(&mut self, closes: &VecDeque<f64>) -> f64 {
        let now = back(closes, 0);
        let then = back(closes, self.bars);
        // No close `bars` back: the change is undefined, and neither average moves.
        let (rise, fall) = if then.is_nan() {
            (f64::NAN, f64::NAN)
        } else {
            ((now - then).max(0.0), (then - now).max(0.0))
        };
        let strength = self.rises.next_value(rise) / self.falls.next_value(fall);
        100.0 - 100.0 / (1.0 + strength)
    }
}

struct S {
    rsi: Rsi,
    first: Layer,
    second: Layer,
    /// The closes, newest first, as many as the longer layer reads.
    closes: VecDeque<f64>,
    rsi_up: Cross,
    rsi_down: Cross,
    first_up: Cross,
    second_up: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        rsi: Rsi::new(LENGTH).expect("a valid length"),
        first: Layer::new(FIRST_LAYER),
        second: Layer::new(SECOND_LAYER),
        closes: VecDeque::new(),
        rsi_up: Cross::new(),
        rsi_down: Cross::new(),
        first_up: Cross::new(),
        second_up: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { pyramiding: 2, ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        push(&mut self.closes, bar.close, SECOND_LAYER + 1);
        let rsi = self.rsi.next(bar);
        let first = self.first.next(&self.closes);
        let second = self.second.next(&self.closes);
        // Every cross steps on every bar, so none compares against a stale value.
        let rsi_up = matches!(self.rsi_up.step(rsi, OVERSOLD), Signal::Long);
        let rsi_down = matches!(self.rsi_down.step(rsi, OVERBOUGHT), Signal::Short);
        let first_up = matches!(self.first_up.step(first, OVERSOLD), Signal::Long);
        let second_up = matches!(self.second_up.step(second, OVERSOLD), Signal::Long);

        let layered = (first_up && second > OVERSOLD && rsi > OVERSOLD)
            || (second_up && first > OVERSOLD && rsi > OVERSOLD);
        let buy = (rsi_up && first < OVERSOLD && second < OVERSOLD) || layered;
        if buy {
            strategy.entry("Long", Long);
        }
        if rsi_down {
            strategy.close("Long");
        }
    }
}
