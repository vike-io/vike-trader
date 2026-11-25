// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "EDMA Scalping Strategy (Exponentially Deviating Moving Average)" by MightyZinger, https://www.tradingview.com/script/CJesxF5m/
// The original's notice: © MightyZinger
// Changed: rewritten by hand in Rust for vike.

//! The source is the bar's high when it closed up and its low otherwise, smoothed by the symmetric 4-bar weighting (1, 2, 2, 1
//! over 6). An exponentially deviating average of it follows its highs at once and its lows slowly (length 20), is smoothed again as
//! a Hull average (13 bars, parts of 6 and 13, result over 4) and by the 4-bar weighting. Two EMAs of the source, 20 and 40, are
//! compared to it. Each side's state is the sign of the EMA less the average, kept through a tie; a change into up is a buy,
//! into down a sell, on the 20-bar EMA alone. A buy closes the short and goes long, a sell the reverse. 5% of equity, 0.1%
//! commission, 10,000 start.
//!
//! The rule imports a library for its colour filter and for the trend it reads off a displaced source; both are off by default
//! (the filter's confirmation is a setting that is off), so the library is not ported: its output reaches only the colours.
//! The rule's two states share one running value inside a loop, as a `var` declared in a loop does; they differ from separate
//! ones only on an exact tie of two averages.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Ema, Hma, Swma};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const LENGTH: f64 = 20.0;

struct S {
    source: Swma,
    outer: Swma,
    hexp: f64,
    lexp: f64,
    /// The Hull average of `lexp`: 13 bars, parts of 6 and 13, the result over `round(sqrt(13))` = 4.
    hull: Hma,
    ema_fast: Ema,
    ema_slow: Ema,
    /// The shared running state of the two loop passes, and the last bar's strong state.
    sig: i32,
    strong_before: i32,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let ema = |n: f64| Ema::new(n).expect("a valid length").with_skip_nan(true);
    Box::new(S {
        source: Swma::default(),
        outer: Swma::default(),
        hexp: f64::NAN,
        lexp: f64::NAN,
        hull: Hma::new(13.0).expect("a valid length").with_rounded_root(true),
        ema_fast: ema(20.0),
        ema_slow: ema(40.0),
        sig: 0,
        strong_before: 0,
    })
}

impl S {
    /// The average at this bar's source.
    fn edma(&mut self, src: f64) -> f64 {
        let k = 1.0 / (LENGTH + 1.0);
        self.hexp = if self.hexp.is_nan() || src >= self.hexp {
            // `na(hexp[1]) ? src : src >= hexp[1] ? src : ...`
            src
        } else {
            self.hexp + (src - self.hexp) * k
        };
        self.lexp = if self.lexp.is_nan() || src <= self.lexp {
            self.hexp
        } else {
            self.lexp + (src - self.lexp) * k
        };
        // `int(len / 1.5)` is 13. A missing value is not fed: it is missing only on the first bars, and the reference's
        // averages are missing while one is in the window.
        if self.lexp.is_nan() {
            return f64::NAN;
        }
        self.hull.next_value(self.lexp)
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.1), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let raw = if bar.close > bar.open { bar.high } else { bar.low };
        let src = self.source.next_value(raw);
        let smoothed = self.edma(src);
        let edma = self.outer.next_value(smoothed);
        let fast = self.ema_fast.next_value(src);
        let slow = self.ema_slow.next_value(src);

        // The loop's two passes, the weak one on the 40-bar EMA and the strong one on the 20-bar EMA.
        let mut strong = 0;
        for ema in [slow, fast] {
            if ema > edma && self.sig <= 0 {
                self.sig = 1;
            }
            if ema < edma && self.sig >= 0 {
                self.sig = -1;
            }
            strong = self.sig;
        }
        let buy = strong == 1 && self.strong_before != 1;
        let sell = strong == -1 && self.strong_before != -1;
        self.strong_before = strong;

        if buy {
            strategy.close("sell");
            strategy.entry("buy", Long).size(share_of_worth(&sizing, 5.0, bar.close));
        }
        if sell {
            strategy.close("buy");
            strategy.entry("sell", Short).size(share_of_worth(&sizing, 5.0, bar.close));
        }
    }
}
