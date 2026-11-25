// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-pivot-atr-stop-target-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 1 when the close crosses up through the last confirmed pivot high, short 1 when it crosses down through the last confirmed
//! pivot low, either one reversing the other side. A pivot has 5 bars on each side. Each signal issues a stop 1.5 ATR(14) and a
//! target 2 ATR(14) from its bar's close, live in the entry bar, and one on the side already held moves them. No costs.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::ConfirmedPivot;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const UP: &str = "long";
const DOWN: &str = "short";
const STOP_ATRS: f64 = 1.5;
const TARGET_ATRS: f64 = 2.0;

struct S {
    pivots: ConfirmedPivot,
    atr: IndicatorLine,
    // NaN until the first pivot of its side, and a NaN compares false.
    last_high: f64,
    last_low: f64,
    last_close: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        pivots: ConfirmedPivot::new(5.0, 5.0).expect("valid spans"),
        atr: IndicatorLine::new("atr", &[14.0]),
        last_high: f64::NAN,
        last_low: f64::NAN,
        last_close: f64::NAN,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        // The newest confirmed pivot of each side, one this bar confirms included.
        let [_, _, newest_high, newest_low] = self.pivots.next(bar);
        let atr = self.atr.next(bar)[0];
        self.last_high = newest_high;
        self.last_low = newest_low;
        // The previous close is judged against the level as it stands now, not as it stood then.
        let before = std::mem::replace(&mut self.last_close, bar.close);
        let (stop, target) = (atr * STOP_ATRS, atr * TARGET_ATRS);
        if bar.close > self.last_high && before <= self.last_high {
            strategy.entry(UP, Long).size(1.0);
            strategy.exit("long exit").from(UP).take(bar.close + target).stop(bar.close - stop);
        }
        if bar.close < self.last_low && before >= self.last_low {
            strategy.entry(DOWN, Short).size(1.0);
            strategy.exit("short exit").from(DOWN).take(bar.close - target).stop(bar.close + stop);
        }
    }
}
