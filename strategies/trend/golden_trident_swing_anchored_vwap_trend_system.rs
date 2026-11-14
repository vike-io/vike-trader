// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Golden Trident | Swing-Anchored VWAP Trend System" by blitz_locked, https://www.tradingview.com/script/Eq4kPrrJ/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2026-09-02, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Long only, 20% of the account's worth, on swing structure. The structure is up when the last bar whose high was the highest
//! of 30 bars came after the last bar whose low was the lowest of 30. Long on the first bar of: structure up, the close above
//! EMA(200), and the 20-bar high-low range wider than 0.8 ATR(20), when not long already, with a stop 8 ATR(14) under that
//! close, issued from the bar after the fill. Closed at the next open once the structure is down. 0.02% commission, 2 ticks
//! of slippage; starts with 10,000.
//!
//! The anchored VWAP the rule draws trades nothing and is left out.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Donchian;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const NAME: &str = "Long";
const SWING: f64 = 30.0;
const RANGE_MULTIPLIER: f64 = 0.8;
const BACKSTOP_ATRS: f64 = 8.0;

struct S {
    /// Where the last 30 bars' highest high and lowest low lie: this bar is the extreme at offset 0, a tie going to the older
    /// bar, as the reference's `highestbars` and `lowestbars` count it.
    swing: Donchian,
    high_at: usize,
    low_at: usize,
    bars: usize,
    trend: IndicatorLine,
    stop_atr: IndicatorLine,
    chop_atr: IndicatorLine,
    window: Donchian,
    was_ready: bool,
    backstop: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        swing: Donchian::new(SWING).expect("a valid length").with_extreme_offsets(true),
        high_at: 0,
        low_at: 0,
        bars: 0,
        trend: IndicatorLine::new("ema", &[200.0]),
        stop_atr: IndicatorLine::new("atr", &[14.0]),
        chop_atr: IndicatorLine::new("atr", &[20.0]),
        window: Donchian::new(20.0).expect("a valid length"),
        was_ready: false,
        backstop: f64::NAN,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.02), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        self.swing.next(bar);
        let offsets = self.swing.extreme_offsets();
        if offsets.is_some_and(|[high, _]| high == 0) {
            self.high_at = self.bars;
        }
        if offsets.is_some_and(|[_, low]| low == 0) {
            self.low_at = self.bars;
        }
        self.bars += 1;
        let up = self.high_at > self.low_at;
        let trend = self.trend.next(bar)[0];
        let stop_atr = self.stop_atr.next(bar)[0];
        let chop_atr = self.chop_atr.next(bar)[0];
        let [highest, _, lowest] = self.window.next(bar);
        let wide = highest - lowest > chop_atr * RANGE_MULTIPLIER;
        let ready = up && bar.close > trend && wide;
        let was_ready = std::mem::replace(&mut self.was_ready, ready);
        let trigger = ready && !was_ready;

        let held = strategy.position();
        if trigger && held <= 0.0 {
            self.backstop = bar.close - stop_atr * BACKSTOP_ATRS;
            let price = sizing.slipped(Long, bar.close);
            strategy.entry(NAME, Long).size(share_of_worth(&sizing, 20.0, price));
        }
        if !up && held > 0.0 {
            strategy.close(NAME);
        }
        if held > 0.0 {
            strategy.exit("Long Backstop").from(NAME).stop(self.backstop);
        }
    }
}
