// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-stop-entry-cancel-opposite-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Every bar: while EMA(8) is above EMA(21), a buy stop at the previous bar's high, pulling the sell stop; otherwise a sell stop at
//! the previous bar's low, pulling the buy stop. Size 1, plus whatever opposite position is held when placed. A long closes at the
//! next open once EMA(8) is under EMA(21), and a short once it is over.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::Ema;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const BUY: &str = "buy";
const SELL: &str = "sell";

struct CancelOpposite {
    fast: Ema,
    slow: Ema,
    previous_high: f64,
    previous_low: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(CancelOpposite {
        fast: Ema::new(8.0).expect("a valid period"),
        slow: Ema::new(21.0).expect("a valid period"),
        previous_high: f64::NAN,
        previous_low: f64::NAN,
    })
}

impl Rule for CancelOpposite {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let fast = self.fast.next(bar);
        let slow = self.slow.next(bar);
        let held = strategy.position();
        if fast > slow {
            strategy.entry(BUY, Long).size(1.0).stop(self.previous_high);
            strategy.cancel(SELL);
        } else {
            strategy.entry(SELL, Short).size(1.0).stop(self.previous_low);
            strategy.cancel(BUY);
        }
        if held > 0.0 && fast < slow {
            strategy.close(BUY);
        }
        if held < 0.0 && fast > slow {
            strategy.close(SELL);
        }
        self.previous_high = bar.high;
        self.previous_low = bar.low;
    }
}
