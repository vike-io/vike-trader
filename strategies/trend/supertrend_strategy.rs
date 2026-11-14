// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "SuperTrend STRATEGY" by holdon_to_profits, https://www.tradingview.com/script/VLRj2sG9/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2026-02-11, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! A supertrend on the median price with bands 8.5 times the 10-bar SMA of the true range: long all of the account's worth
//! when the trend turns up, closed when it turns down. Orders fill at the close of the bar that placed them. 0.015%
//! commission; starts with 10,000,000.
//!
//! The lower band only rises while the previous close is above its previous value, the upper only falls while the previous
//! close is below its own; the trend turns up on a close above the previous upper band and down on one below the previous
//! lower band, each turn judged only against the band the trend would cross, and a band the last bar lacked read as this
//! bar's: vike's `Supertrend` with `Supertrend::with_previous_bands` and `Supertrend::with_gated_turns`, about the midpoint
//! with this ATR (`Supertrend::next_centred`).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Sma, Supertrend};
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const MULTIPLIER: f64 = 8.5;
const NAME: &str = "buy";

struct S {
    range: IndicatorLine,
    atr: Sma,
    line: Supertrend,
    /// `1` up, `-1` down, NaN before the first bar.
    trend: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        range: IndicatorLine::new("true_range", &[]),
        atr: Sma::new(10.0).expect("a valid length").with_fresh_sum(true),
        line: Supertrend::new(10.0, MULTIPLIER)
            .expect("valid parameters")
            .with_previous_bands(true)
            .with_gated_turns(true),
        trend: f64::NAN,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.015), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let atr = self.atr.next_value(self.range.next(bar)[0]);
        let mid = (bar.high + bar.low) / 2.0;
        let trend = self.line.next_centred(bar.close, mid, atr)[1];
        if trend == 1.0 && self.trend == -1.0 {
            strategy.entry(NAME, Long).size(share_of_worth(&sizing, 100.0, bar.close));
        } else if trend == -1.0 && self.trend == 1.0 {
            strategy.close(NAME);
        }
        self.trend = trend;
    }
}
