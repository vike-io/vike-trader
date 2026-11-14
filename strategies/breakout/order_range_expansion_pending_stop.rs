// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-range-expansion-pending-stop-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! When the previous bar's range beat the previous bar's ATR(14): after a rising bar a buy stop at its high, after a falling bar a
//! sell stop at its low, size 1 plus whatever opposite position is held when placed. Each stop is moved every bar its condition
//! holds and pulled on the first bar it does not.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::Atr;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const BUY: &str = "buy";
const SELL: &str = "sell";

struct RangeExpansion {
    atr: Atr,
    previous_atr: f64,
    previous: Option<Bar>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(RangeExpansion {
        atr: Atr::new(14.0).expect("a valid period"),
        previous_atr: f64::NAN,
        previous: None,
    })
}

impl Rule for RangeExpansion {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let atr = self.atr.next(bar);
        let previous_atr = std::mem::replace(&mut self.previous_atr, atr);
        let Some(previous) = self.previous.replace(bar.clone()) else { return };
        // A NaN compares false, so nothing arms while the ATR warms up.
        let expanded = previous.high - previous.low > previous_atr;
        if expanded && previous.close > previous.open {
            strategy.entry(BUY, Long).size(1.0).stop(previous.high);
        } else {
            strategy.cancel(BUY);
        }
        if expanded && previous.close < previous.open {
            strategy.entry(SELL, Short).size(1.0).stop(previous.low);
        } else {
            strategy.cancel(SELL);
        }
    }
}
