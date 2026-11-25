// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "PPS" by Julien_Exe, https://www.tradingview.com/script/WpJ5ym7u/
// The original's notice: © Julien_Eche
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2023-08-11, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Two trailing trends, either of which turning gives a signal. The first is a SuperTrend around a centre line of pivot points
//! (2 bars each side): each new pivot high or low moves the centre two thirds of the way from the pivot to where it was, the
//! bands are the centre ± 3 ATR(10), and the trend turns when the close passes the last bar's band. The second is a plain
//! SuperTrend around the bar's midpoint, ± 3 ATR(10), counting only when the close agrees with SMA(20): above it for a turn up,
//! below it for a turn down. Both are vike's `Supertrend` judged against the last bar's bands (`Supertrend::with_previous_bands`):
//! the first about its centre (`Supertrend::next_centred`), the second with each turn judged only against the band the trend
//! would cross and a band the last bar lacked read as this bar's (`Supertrend::with_gated_turns`). A turn up buys 50,000 worth; a turn down closes the long and sells 50,000 worth short, which only
//! the next turn up closes. No costs; starts with 500,000 in a USD account.
//!
//! The rule's date window (2018-09-01 to 9999) holds on every bar here.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Atr, ConfirmedPivot, Sma, Supertrend};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::helpers::truthy;
use vike_tactics::sizing::cash_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const CASH: f64 = 50_000.0;
const FACTOR: f64 = 3.0;

struct S {
    pivots: ConfirmedPivot,
    atr: Atr,
    average: Sma,
    center: f64,
    /// The pivot trend, and its direction as the last bar left it.
    pivot_line: Supertrend,
    pivot_trend: Option<i32>,
    /// The plain trend, and its direction as the last bar left it.
    line: Supertrend,
    trend: Option<i32>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        pivots: ConfirmedPivot::new(2.0, 2.0).expect("valid lengths"),
        atr: Atr::new(10.0).expect("a valid length"),
        average: Sma::new(20.0).expect("a valid length"),
        center: f64::NAN,
        pivot_line: Supertrend::new(10.0, FACTOR)
            .expect("valid parameters")
            .with_previous_bands(true),
        pivot_trend: None,
        line: Supertrend::new(10.0, FACTOR)
            .expect("valid parameters")
            .with_previous_bands(true)
            .with_gated_turns(true),
        trend: None,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let [ph, pl, _, _] = self.pivots.next(bar);
        let atr = self.atr.next(bar);
        let average = self.average.next(bar);

        let lastpp = if truthy(ph) {
            ph
        } else if truthy(pl) {
            pl
        } else {
            f64::NAN
        };
        if truthy(lastpp) {
            self.center =
                if self.center.is_nan() { lastpp } else { (self.center * 2.0 + lastpp) / 3.0 };
        }
        let pivot_trend =
            if self.pivot_line.next_centred(bar.close, self.center, atr)[1] > 0.0 { 1 } else { -1 };
        let last_pivot_trend = self.pivot_trend.replace(pivot_trend);
        let bsignal = pivot_trend == 1 && last_pivot_trend == Some(-1);
        let ssignal = pivot_trend == -1 && last_pivot_trend == Some(1);

        let trend = if self.line.next(bar)[1] > 0.0 { 1 } else { -1 };
        let last_trend = self.trend.replace(trend);

        let long = (trend == 1 && last_trend == Some(-1) && bar.close > average) || bsignal;
        let short = (trend == -1 && last_trend == Some(1) && bar.close < average) || ssignal;
        if long {
            strategy.entry("BUY", Long).size(cash_worth(&sizing, CASH, bar.close));
        }
        if short {
            strategy.close("BUY");
            strategy.entry("SELL", Short).size(cash_worth(&sizing, CASH, bar.close));
        }
    }
}
