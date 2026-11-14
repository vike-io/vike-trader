// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "PMax on Rsi w T3 Strategy" by lucidreamer15, https://www.tradingview.com/script/ZLcmWQtj/
// The original's notice: © KivancOzbilgic
// Changed: rewritten by hand in Rust for vike.

//! A Tillson T3 (8 bars, volume factor 0.7) of a 14-bar RSI, and a profit-maximizer line 3 "ATRs" of the RSI below it while
//! it trends up and above it while it trends down, each side ratcheting with the T3. Long 1 when the T3 crosses up through
//! that line, short 1 when it crosses down, each reversing the other. No costs; starts with 100,000.
//!
//! The RSI is vike's `Rsi` in the RS form with both means started from 0 on the first bar, and the T3 the expanded form
//! over six straight-chained EMAs (`T3::with_six_emas`), each skipping the NaN values before its input starts, and the line
//! vike's `Supertrend` about the T3 with the T3 as its price (`Supertrend::next_centred`), judged against the last bar's bands
//! with gated turns and carried on this bar's price (`Supertrend::with_previous_bands`, `Supertrend::with_gated_turns`,
//! `Supertrend::with_carry_on_close`); the rest is the rule's own and written here. Its "true range" is the widest of three
//! RSIs read off the 14-bar SMAs of gains and losses with the bar's high and low standing in for the close, against each
//! other and the last RSI, averaged over 10 bars. The rule's date range (2005 to 9999) holds on every bar.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Rsi, Supertrend, T3};
use vike_indicators::strategy_kit::max;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::rolling::RollingSum;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const RSI_LENGTH: usize = 14;
const ATR_LENGTH: usize = 10;
const T3_LENGTH: f64 = 8.0;
const T3_FACTOR: f64 = 0.7;
const MULTIPLIER: f64 = 3.0;

/// `100 - 100 / (1 + up / down)`, the rule's guard against a ratio of -1 included.
fn index(up: f64, down: f64) -> f64 {
    let rs = up / down;
    if rs == -1.0 { 0.0 } else { 100.0 - 100.0 / (1.0 + rs) }
}

struct S {
    last_close: f64,
    rsi: Rsi,
    up_sum: RollingSum,
    down_sum: RollingSum,
    last_rsi: f64,
    range_sum: RollingSum,
    t3: T3,
    stop: Supertrend,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        last_close: f64::NAN,
        rsi: Rsi::new(RSI_LENGTH as f64)
            .expect("a valid length")
            .with_rs_form(true)
            .with_zero_seed(true),
        up_sum: RollingSum::new(RSI_LENGTH),
        down_sum: RollingSum::new(RSI_LENGTH),
        last_rsi: f64::NAN,
        range_sum: RollingSum::new(ATR_LENGTH),
        t3: T3::new(T3_LENGTH, T3_FACTOR).expect("valid parameters").with_six_emas(true),
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
        let (close, last) = (bar.close, self.last_close);
        self.last_close = close;
        // A comparison with no last close is false, so the first bar gains and loses 0.
        let gain = if close >= last { close - last } else { 0.0 };
        let loss = if close < last { last - close } else { 0.0 };
        let n = RSI_LENGTH as f64;
        let up_mean = self.up_sum.next(gain) / n;
        let down_mean = self.down_sum.next(loss) / n;
        let away = |x: f64| if x > last { x - last } else { 0.0 };
        let under = |x: f64| if x < last { last - x } else { 0.0 };
        let with_last = |mean: f64, k: f64| (mean * (n - 1.0) + k) / n;
        let rsi = self.rsi.next_value(close);
        let rsi_high =
            index(with_last(up_mean, away(bar.high)), with_last(down_mean, under(bar.high)));
        let rsi_low =
            index(with_last(up_mean, away(bar.low)), with_last(down_mean, under(bar.low)));
        let range = max(
            max(rsi_high - rsi_low, (rsi_high - self.last_rsi).abs()),
            (rsi_low - self.last_rsi).abs(),
        );
        self.last_rsi = rsi;
        let atr = self.range_sum.next(range) / ATR_LENGTH as f64;

        let average = self.t3.next_value(rsi);

        let [pmax, _] = self.stop.next_centred(average, average, atr);

        match self.cross.step(average, pmax) {
            Signal::Long => {
                strategy.entry("long", Long).size(1.0);
            }
            Signal::Short => {
                strategy.entry("short", Short).size(1.0);
            }
            _ => {}
        }
    }
}
