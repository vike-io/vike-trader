// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "ABCD Strategy" by kerok3g, https://www.tradingview.com/script/20aoz32a/
// The original's notice: © kerok3g
// Changed: rewritten by hand in Rust for vike.

//! An ABCD pattern from pivots of 5 bars a side: the newest two pivot highs and the newest two pivot lows, a bar confirming both
//! keeping only the high. Long 1 when they alternate high, low, high, low (oldest first), the second high retraces the first
//! leg by 0.5 to 0.786 of it and extends the second by 1 to 1.414, and the close lies between the newer low and the older high;
//! short 1 on the mirror (extension 1.1 to 1.414). Each reverses the other, once per pattern. The long carries a stop at the
//! newer low less this bar's true range and a target 1.5 times that distance above the close, capped at the older high; the
//! short mirrors it. A signal on the side already held adds nothing and moves that exit. 0.04% commission; starts with
//! 1,000,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{ConfirmedPivot, TrueRange};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const SPAN: i64 = 5;

/// A pivot's price and the index of its bar, both 0 before the first, as the rule starts them.
#[derive(Clone, Copy, Default)]
struct Pivot {
    price: f64,
    at: i64,
}

struct S {
    pivots: ConfirmedPivot,
    bar: i64,
    range: TrueRange,
    /// Newest first.
    highs: [Pivot; 2],
    lows: [Pivot; 2],
    signal: i64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        pivots: ConfirmedPivot::new(SPAN as f64, SPAN as f64).expect("valid spans"),
        bar: -1,
        range: TrueRange::default(),
        highs: [Pivot::default(); 2],
        lows: [Pivot::default(); 2],
        signal: 0,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.04), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        self.bar += 1;
        let [ph, pl, _, _] = self.pivots.next(bar);
        let at = self.bar - SPAN;
        if !ph.is_nan() {
            self.highs = [Pivot { price: ph, at }, self.highs[0]];
        } else if !pl.is_nan() {
            self.lows = [Pivot { price: pl, at }, self.lows[0]];
        }
        let tr = self.range.next(bar);
        let [ch, oh] = self.highs;
        let [cl, ol] = self.lows;
        let c = bar.close;

        let form_long = oh.at < ol.at && ol.at < ch.at && ch.at < cl.at;
        let r1 = (ch.price - ol.price) / (oh.price - ol.price);
        let r2 = (ch.price - cl.price) / (ch.price - ol.price);
        if form_long
            && r1 < 0.786
            && r1 > 0.5
            && r2 > 1.0
            && r2 < 1.414
            && c < oh.price
            && c > cl.price
            && self.signal != ol.at
        {
            self.signal = ol.at;
            let stop = cl.price - tr;
            let target = ((c - stop) * 1.5) + c;
            strategy.entry("Long", Long).size(1.0);
            strategy.exit("Exit Long").from("Long").take(target.min(oh.price)).stop(stop);
        }

        let form_short = ol.at < oh.at && oh.at < cl.at && cl.at < ch.at;
        let s1 = (oh.price - cl.price) / (oh.price - ol.price);
        let s2 = (ch.price - cl.price) / (oh.price - cl.price);
        if form_short
            && s1 < 0.786
            && s1 > 0.5
            && s2 > 1.1
            && s2 < 1.414
            && c > ol.price
            && c < ch.price
            && self.signal != oh.at
        {
            self.signal = oh.at;
            let stop = ch.price + tr;
            let target = c - ((stop - c) * 1.5);
            strategy.entry("Short", Short).size(1.0);
            strategy.exit("Exit Short").from("Short").take(target.max(ol.price)).stop(stop);
        }
    }
}
