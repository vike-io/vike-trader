// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Profit Maximizer Strategy Long-Short" by melihtuna, https://www.tradingview.com/script/v6Ws9H9j/
// The original's notice: © melihtuna
// Changed: rewritten by hand in Rust for vike.

//! PMax: a trailing stop 3 ATR(10) from EMA(10) of the bar's midpoint, under it while the trend is up and over it while down,
//! each side only ever moving with the average. The trend turns up when the average passes over the last bar's upper stop and
//! down when it passes under the last bar's lower stop. Long 10,000 worth when the average crosses up through PMax; closed when
//! it crosses down. No costs; starts with 10,000 in a USD account. The rule's default is long only, on the crossing of the
//! average.
//!
//! The stop line is vike's `Supertrend` about the average with the average as its price (`Supertrend::next_centred`), judged
//! against the last bar's bands with gated turns and carried on this bar's price (`Supertrend::with_previous_bands`,
//! `Supertrend::with_gated_turns`, `Supertrend::with_carry_on_close`).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Atr, Ema, Supertrend};
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::cash_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "BUY";
const CASH: f64 = 10_000.0;
const MULTIPLIER: f64 = 3.0;

struct S {
    average: Ema,
    atr: Atr,
    stop: Supertrend,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        average: Ema::new(10.0).expect("a valid length"),
        atr: Atr::new(10.0).expect("a valid length"),
        stop: Supertrend::new(10.0, MULTIPLIER)
            .expect("valid parameters")
            .with_previous_bands(true)
            .with_gated_turns(true)
            .with_carry_on_close(true),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let average = self.average.next_value((bar.high + bar.low) / 2.0);
        let atr = self.atr.next(bar);
        let [pmax, _] = self.stop.next_centred(average, average, atr);
        match self.cross.step(average, pmax) {
            Signal::Long => {
                strategy.entry(NAME, Long).size(cash_worth(&sizing, CASH, bar.close));
            }
            Signal::Short => {
                strategy.close(NAME);
            }
            _ => {}
        }
    }
}
