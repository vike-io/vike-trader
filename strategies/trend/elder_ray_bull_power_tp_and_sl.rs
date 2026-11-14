// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Elder Ray (Bull Power) TP and SL" by HPotter, https://www.tradingview.com/script/Cv3YP9Tw/
// The original's notice: Copyright by HPotter v1.0 06/10/2022
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2022-10-06, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Short 1 at the next open while flat when the day's high so far (from 00:00 UTC) is more than 200 under EMA(14) of the
//! close, with a target 7% under this close and a stop 7% over it, both placed with the entry. No costs; starts with
//! 1,000,000.
//!
//! The rule's signal is "reversed": it would go long on the other condition, which it never computes, so only the short
//! side can trade.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::MS_PER_DAY;
use vike_model::OrderSide::Short;
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "Short";
const LENGTH: f64 = 14.0;
const TRIGGER: f64 = -200.0;
const PROFIT: f64 = 7.0;
const STOP: f64 = 7.0;

struct S {
    ema: IndicatorLine,
    day: i64,
    day_high: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S { ema: IndicatorLine::new("ema", &[LENGTH]), day: i64::MIN, day_high: f64::NAN })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let ema = self.ema.next(bar)[0];
        let day = bar.timestamp.div_euclid(MS_PER_DAY);
        self.day_high = if day != self.day { bar.high } else { bar.high.max(self.day_high) };
        self.day = day;
        if self.day_high - ema < TRIGGER && strategy.position() == 0.0 {
            let c = bar.close;
            strategy.entry(NAME, Short).size(1.0);
            strategy
                .exit("ExitShort")
                .from(NAME)
                .take(c - c * PROFIT / 100.0)
                .stop(c + c * STOP / 100.0);
        }
    }
}
