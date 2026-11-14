// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "B-Xtrender [Backtest Edition] @QuantTherapy" by QuantTherapy, https://www.tradingview.com/script/uCWPAPT2/
// The original's notice: © QuantTherapy
// Changed: rewritten by hand in Rust for vike.

//! Two RSI-of-EMA oscillators, less 50: the short-term one is RSI(15) of EMA(5) - EMA(20), the long-term one RSI(15) of EMA(20).
//! Long 1 when both are above 0 and the close is over its EMA(200); closed when either is under 0. Short 1 when both are under
//! 0 and the close is under its EMA(200); closed when either is over 0. Each side reverses the other. While held, a stop 20% of
//! the close away from it that only ever moves toward the price: a long's is the highest close less 20% since the position
//! opened, a short's is the lowest close plus 20%, or a million ticks on its first bar: where that is under the price, as
//! 10,000 is at a tick of 0.01 under BTCUSDT's, a short's stop is met at once. No costs; starts with 100,000. The rule's date
//! window and trade direction hold on every bar.
//!
//! The RSI is the reference's (vike's in the RS form, `Rsi::with_rs_form`), fed the EMA's difference once it exists: the form
//! skips the `NaN`s before it.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Ema, Rsi};
use vike_indicators::strategy_kit::{max, min};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

/// The stop's distance from the close, in percent.
const STOP_PERCENT: f64 = 20.0;
/// What a short's stop reads while no short is held, in ticks of the instrument: `mintick * 1000000`.
const SHORT_STOP_IDLE_TICKS: f64 = 1_000_000.0;

struct S {
    short_fast: Ema,
    short_slow: Ema,
    short_rsi: Rsi,
    long_ema: Ema,
    long_rsi: Rsi,
    trend: Ema,
    long_stop: f64,
    short_stop: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let ema = |n: f64| Ema::new(n).expect("a valid length").with_skip_nan(true);
    let rsi = || Rsi::new(15.0).expect("a valid length").with_rs_form(true);
    Box::new(S {
        short_fast: ema(5.0),
        short_slow: ema(20.0),
        short_rsi: rsi(),
        long_ema: ema(20.0),
        long_rsi: rsi(),
        trend: ema(200.0),
        long_stop: 0.0,
        // the idle stop, set on the first bar, once the tick is known
        short_stop: f64::NAN,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let idle = SHORT_STOP_IDLE_TICKS * tick;
        if self.short_stop.is_nan() {
            self.short_stop = idle;
        }
        let c = bar.close;
        let diff = self.short_fast.next_value(c) - self.short_slow.next_value(c);
        let short_term = self.short_rsi.next_value(diff) - 50.0;
        let long_term = self.long_rsi.next_value(self.long_ema.next_value(c)) - 50.0;
        let ma = self.trend.next_value(c);

        let long = short_term > 0.0 && long_term > 0.0 && c > ma;
        let close_long = short_term < 0.0 || long_term < 0.0;
        let short = short_term < 0.0 && long_term < 0.0 && c < ma;
        let close_short = short_term > 0.0 || long_term > 0.0;
        if long {
            strategy.entry("Long", Long).size(1.0);
        }
        if close_long {
            strategy.close("Long");
        }
        if short {
            strategy.entry("Short", Short).size(1.0);
        }
        if close_short {
            strategy.close("Short");
        }

        let held = strategy.position();
        let distance = c * STOP_PERCENT / 100.0;
        self.long_stop = if held > 0.0 { max(c - distance, self.long_stop) } else { 0.0 };
        self.short_stop = if held < 0.0 { min(c + distance, self.short_stop) } else { idle };
        if held > 0.0 {
            strategy.exit("TSL Long").from("Long").stop(self.long_stop);
        }
        if held < 0.0 {
            strategy.exit("TSL Short").from("Short").stop(self.short_stop);
        }
    }
}
