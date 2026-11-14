// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "EMA Cross + RSI + ADX - Strategy V2" by varuns_back, https://www.tradingview.com/script/e7XQPek8/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2025-11-10, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Long 10 when EMA(9) of the close crosses up through EMA(21) with RSI(14) over 55 and the ADX(14) over 0, short 10 when it
//! crosses down with RSI under 40 and the same ADX test, each reversing the other. Each entry sets a stop 2% from the signal
//! bar's close; a position closes (at the next open) on the opposite cross, or when a bar's low (a long) or high (a short)
//! touches the stop. The calls go entries, closes on the cross, stop hits. 0.075% commission; starts with 1,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Adx, Ema, Rsi};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::floor_printed;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const SIZE: f64 = 10.0;
const STOP_PERCENT: f64 = 2.0;
const NAMES: [&str; 2] = ["LONG", "SHORT"];

struct S {
    fast: Ema,
    slow: Ema,
    rsi: Rsi,
    adx: Adx,
    cross: Cross,
    long_stop: f64,
    short_stop: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: Ema::new(9.0).expect("a valid length"),
        slow: Ema::new(21.0).expect("a valid length"),
        rsi: Rsi::new(14.0).expect("a valid length").with_rs_form(true),
        adx: Adx::new(14.0).expect("a valid length"),
        cross: Cross::new(),
        long_stop: f64::NAN,
        short_stop: f64::NAN,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.075), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let signal = self.cross.step(self.fast.next(bar), self.slow.next(bar));
        let rsi = self.rsi.next_value(bar.close);
        let [adx, _, _] = self.adx.next(bar);
        let adx_ok = adx > 0.0;
        let long = matches!(signal, Signal::Long) && rsi > 55.0 && adx_ok;
        let short = matches!(signal, Signal::Short) && rsi < 40.0 && adx_ok;
        if long {
            self.long_stop = bar.close * (1.0 - STOP_PERCENT / 100.0);
        }
        if short {
            self.short_stop = bar.close * (1.0 + STOP_PERCENT / 100.0);
        }
        let held = strategy.position();
        let long_hit = held > 0.0 && bar.low <= self.long_stop;
        let short_hit = held < 0.0 && bar.high >= self.short_stop;
        let size = floor_printed(SIZE, sizing.qty_step());
        if long {
            strategy.entry(NAMES[0], Long).size(size);
        }
        if short {
            strategy.entry(NAMES[1], Short).size(size);
        }
        if matches!(signal, Signal::Short) && held > 0.0 {
            strategy.close(NAMES[0]);
            self.long_stop = f64::NAN;
        }
        if matches!(signal, Signal::Long) && held < 0.0 {
            strategy.close(NAMES[1]);
            self.short_stop = f64::NAN;
        }
        if long_hit {
            strategy.close(NAMES[0]);
            self.long_stop = f64::NAN;
        }
        if short_hit {
            strategy.close(NAMES[1]);
            self.short_stop = f64::NAN;
        }
    }
}
