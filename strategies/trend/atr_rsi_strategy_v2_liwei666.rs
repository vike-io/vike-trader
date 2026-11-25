// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "ATR_RSI_Strategy v2[liwei666]" by liwei666, https://www.tradingview.com/script/mqh6UJ9p/
// The original's notice: © liwei666
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2023-01-04, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! When flat, with the 45-bar SMA of ATR(26) between 0.3% and 0.7% of the close and ATR(26) above that SMA: long 10% of the
//! account's worth on a buy limit 5 above the close when RSI(15) is over 60, else short 10% on a sell limit 5 under it when RSI(15)
//! is under 40. Each limit rests until it fills or the same entry is placed again. Held long, from the bar after the fill, a stop
//! 1.5% under the highest high since the bar before the fill and a target 3% over the average cost, set again at every close;
//! held short, a stop 1.5% over the lowest low and a target 6% under the cost. 0.05% commission; starts with 10,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Atr, Rsi, Sma};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const UP: &str = "B1";
const DOWN: &str = "S1";
const SHARE: f64 = 10.0;
const OFFSET: f64 = 5.0;
const TRAIL: f64 = 1.5;

struct S {
    atr: Atr,
    atr_ma: Sma,
    rsi: Rsi,
    high: f64,
    low: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        atr: Atr::new(26.0).expect("a valid length"),
        atr_ma: Sma::new(45.0).expect("a valid length"),
        rsi: Rsi::new(15.0).expect("a valid length"),
        high: 0.0,
        low: 0.0,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let atr = self.atr.next(bar);
        // Fed only from the ATR's first number: the SMA's running sum would keep a NaN.
        let atr_ma = if atr.is_nan() { f64::NAN } else { self.atr_ma.next_value(atr) };
        let rsi = self.rsi.next(bar);
        let norm = atr_ma / bar.close * 100.0;
        let held = strategy.position();
        if held == 0.0 {
            (self.high, self.low) = (bar.high, bar.low);
            if (0.3..=0.7).contains(&norm) && atr > atr_ma {
                if rsi > 60.0 {
                    let level = bar.close + OFFSET;
                    strategy
                        .entry(UP, Long)
                        .size(share_of_worth(&sizing, SHARE, bar.close))
                        .limit(level);
                } else if rsi < 40.0 {
                    let level = bar.close - OFFSET;
                    strategy
                        .entry(DOWN, Short)
                        .size(share_of_worth(&sizing, SHARE, bar.close))
                        .limit(level);
                }
            }
        } else if held > 0.0 {
            (self.high, self.low) = (self.high.max(bar.high), bar.low);
            let cut = self.high * (1.0 - TRAIL / 100.0);
            let take = strategy.avg_cost() * 1.03;
            strategy.exit("Exit B1").from(UP).take(take).stop(cut);
        } else {
            (self.high, self.low) = (bar.high, self.low.min(bar.low));
            let cut = self.low * (1.0 + TRAIL / 100.0);
            let take = strategy.avg_cost() * 0.94;
            strategy.exit("Exit S1").from(DOWN).take(take).stop(cut);
        }
    }
}
