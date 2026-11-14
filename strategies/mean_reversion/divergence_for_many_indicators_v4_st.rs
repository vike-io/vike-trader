// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Divergence for Many Indicators v4 ST" by CannyTolany01, https://www.tradingview.com/script/4CIWtqoT/
// The original's notice: © LonesomeTheBlue
// Changed: rewritten by hand in Rust for vike.

//! Regular divergences of eight indicators against the close's pivots: long 1 when any shows a positive one, short 1 when any
//! shows a negative one, each reversing the other. The rule's exits sit at 100% from the average cost (a stop at 0, a target
//! at twice it) and never fire, so they are left out. No costs; starts with 100,000.
//!
//! Pivots are the close's, 9 bars each side. The indicators: MACD(12, 26, 9) and its histogram, RSI(14), SMA(3) of the
//! 14-bar stochastic, CCI(10) of the close, 10-bar momentum, on-balance volume, and VWMA(12) - VWMA(26).
//!
//! A positive divergence, judged when the indicator or the close rose this bar: over the last 10 pivot lows within 100 bars
//! (newest first), the first whose bar, `n` bars back with `n > 5`, has the indicator under last bar's while last bar's close is
//! under that pivot, and where the straight lines from there to last bar, of the indicator and of the close, stay under every
//! bar between. A negative one is the mirror on the pivot highs.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Cci, ConfirmedPivot, Macd, Mom, Obv, Rsi, Stochastic, Vwma};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::helpers::nan_to_zero;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const PRD: i64 = 9;
const MAX_PIVOTS: usize = 10;
const MAX_BARS: i64 = 100;
const KEPT: usize = 20;
const INDICATORS: usize = 8;

/// A series' history, newest last; `at(k)` is the value `k` bars back, NaN before the first.
#[derive(Default)]
struct Hist(Vec<f64>);

impl Hist {
    fn at(&self, k: usize) -> f64 {
        if k < self.0.len() { self.0[self.0.len() - 1 - k] } else { f64::NAN }
    }
}

/// Pivots confirmed, newest first: `(the bar that confirmed it, its close)`, `KEPT` long, `(0, 0.0)` for none yet.
struct Pivots(Vec<(i64, f64)>);

impl Pivots {
    fn new() -> Self {
        Pivots(vec![(0, 0.0); KEPT])
    }

    fn add(&mut self, at: i64, value: f64) {
        self.0.insert(0, (at, value));
        self.0.truncate(KEPT);
    }
}

/// The bars back to the pivot that makes a divergence of `src`, or 0. `low` picks a positive divergence on the pivot lows,
/// otherwise a negative one on the pivot highs.
fn divergence(src: &Hist, close: &Hist, pivots: &Pivots, now: i64, low: bool) -> i64 {
    // The positive test, and the negative one as its mirror: `sign` flips every comparison.
    let sign = if low { 1.0 } else { -1.0 };
    let up = |a: f64, b: f64| if low { a > b } else { a < b };
    if !(up(src.at(0), src.at(1)) || up(close.at(0), close.at(1))) {
        return 0;
    }
    for &(at, value) in pivots.0.iter().take(MAX_PIVOTS) {
        let len = now - at + PRD;
        if at == 0 || len > MAX_BARS {
            break;
        }
        let n = len as usize;
        if !(len > 5 && up(src.at(1), src.at(n)) && up(value, close.at(1))) {
            continue;
        }
        let steps = (len - 1) as f64;
        let slope1 = (src.at(1) - src.at(n)) / steps;
        let mut line1 = src.at(1) - slope1;
        let far_close = if low { close.at(n) } else { nan_to_zero(close.at(n)) };
        let slope2 = (close.at(1) - far_close) / steps;
        let mut line2 = close.at(1) - slope2;
        let mut arrived = true;
        for y in 2..n {
            if sign * src.at(y) < sign * line1 || sign * nan_to_zero(close.at(y)) < sign * line2 {
                arrived = false;
                break;
            }
            line1 -= slope1;
            line2 -= slope2;
        }
        if arrived {
            return len;
        }
    }
    0
}

struct S {
    bar: i64,
    pivot: ConfirmedPivot,
    highs: Pivots,
    lows: Pivots,
    close: Hist,
    lines: [Hist; INDICATORS],
    macd: Macd,
    rsi: Rsi,
    stoch: Stochastic,
    cci: Cci,
    momentum: Mom,
    obv: Obv,
    fast: Vwma,
    slow: Vwma,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        bar: -1,
        pivot: ConfirmedPivot::new(PRD as f64, PRD as f64).expect("valid lengths"),
        highs: Pivots::new(),
        lows: Pivots::new(),
        close: Hist::default(),
        lines: Default::default(),
        macd: Macd::new(12.0, 26.0, 9.0).expect("valid lengths"),
        rsi: Rsi::new(14.0).expect("a valid length"),
        stoch: Stochastic::new(14.0, 3.0, 1.0).expect("valid lengths").with_fresh_sum(true),
        cci: Cci::new(10.0).expect("a valid length"),
        momentum: Mom::new(10.0).expect("a valid length"),
        obv: Obv::default(),
        fast: Vwma::new(12.0).expect("a valid length"),
        slow: Vwma::new(26.0).expect("a valid length"),
    })
}

impl S {
    fn values(&mut self, bar: &Bar) -> [f64; INDICATORS] {
        let [macd, _, hist] = self.macd.next(bar);
        let rsi = self.rsi.next(bar);
        let stoch = self.stoch.next(bar)[0];
        let cci = self.cci.next_hlc(bar.close, bar.close, bar.close);
        let momentum = self.momentum.next_value(bar.close);
        let obv = self.obv.next(bar);
        let vwma = self.fast.next(bar) - self.slow.next(bar);
        [macd, hist, rsi, stoch, cci, momentum, obv, vwma]
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        self.bar += 1;
        let values = self.values(bar);
        self.close.0.push(bar.close);
        for (line, v) in self.lines.iter_mut().zip(values) {
            line.0.push(v);
        }
        let [ph, pl, _, _] = self.pivot.next_hl(bar.close, bar.close);
        if !ph.is_nan() {
            self.highs.add(self.bar, ph);
        }
        if !pl.is_nan() {
            self.lows.add(self.bar, pl);
        }
        let any = |low: bool| {
            let pivots = if low { &self.lows } else { &self.highs };
            self.lines.iter().any(|l| divergence(l, &self.close, pivots, self.bar, low) > 0)
        };
        let (positive, negative) = (any(true), any(false));
        if positive {
            strategy.entry("buy", Long).size(1.0);
        }
        if negative {
            strategy.entry("sell", Short).size(1.0);
        }
    }
}
