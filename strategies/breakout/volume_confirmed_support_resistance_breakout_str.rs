// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Volume-Confirmed Support/Resistance Breakout Strategy" by blitz_locked, https://www.tradingview.com/script/CItvSvqk/
// Changed: rewritten by hand in Rust for vike.

//! Resistance is the latest pivot high and support the latest pivot low, 10 bars either side. Long a tenth of the account's
//! worth when the close crosses up through the resistance on a volume above 1.5 times its 20-bar SMA with the close above
//! EMA(200); short on the mirror through the support below EMA(200); each reverses the other, and none opens after 2025-12-31
//! 00:00 UTC. A long's stop is the support less 0.25 ATR(14) (the close less 1.5 ATR(14) with no support yet) and its target
//! the close plus 3 ATR(14), set at the signal and set again by any later long signal; a short mirrors it. Each side's exit is
//! issued on every bar it is held, from the bar after the fill, and both sides' levels are cleared on the first flat bar after
//! a position. 0.01% commission, 2 ticks; starts with 10,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::ConfirmedPivot;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::helpers::known;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const UP: &str = "long";
const DOWN: &str = "short";
/// 2025-12-31 00:00 UTC, the last bar an entry may be signalled on.
const END_MS: i64 = 1_767_139_200_000;

struct S {
    pivots: ConfirmedPivot,
    volume: IndicatorLine,
    ema: IndicatorLine,
    atr: IndicatorLine,
    resistance: f64,
    support: f64,
    up: Cross,
    down: Cross,
    /// `(stop, target)` of each side, NaN when unset.
    long_levels: (f64, f64),
    short_levels: (f64, f64),
    was_held: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        pivots: ConfirmedPivot::new(10.0, 10.0).expect("valid strengths"),
        volume: IndicatorLine::new("sma", &[20.0]),
        ema: IndicatorLine::new("ema", &[200.0]),
        atr: IndicatorLine::new("atr", &[14.0]),
        resistance: f64::NAN,
        support: f64::NAN,
        up: Cross::new(),
        down: Cross::new(),
        long_levels: (f64::NAN, f64::NAN),
        short_levels: (f64::NAN, f64::NAN),
        was_held: false,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.01), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let [high, low, _, _] = self.pivots.next(bar);
        if !high.is_nan() {
            self.resistance = high;
        }
        if !low.is_nan() {
            self.support = low;
        }
        let volume_ma = self.volume.next(&Bar { close: bar.volume, ..bar.clone() })[0];
        let surge = bar.volume > volume_ma * 1.5;
        let ema = self.ema.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        let in_range = bar.timestamp <= END_MS;
        let crossed_up = matches!(self.up.step(bar.close, self.resistance), Signal::Long);
        let crossed_down = matches!(self.down.step(bar.close, self.support), Signal::Short);
        if crossed_up && surge && bar.close > ema && in_range {
            strategy.entry(UP, Long).size(share_of_worth(
                &sizing,
                10.0,
                sizing.slipped(Long, bar.close),
            ));
            let stop = if self.support.is_nan() {
                bar.close - atr * 1.5
            } else {
                self.support - atr * 0.25
            };
            self.long_levels = (stop, bar.close + atr * 3.0);
        }
        if crossed_down && surge && bar.close < ema && in_range {
            strategy.entry(DOWN, Short).size(share_of_worth(
                &sizing,
                10.0,
                sizing.slipped(Short, bar.close),
            ));
            let stop = if self.resistance.is_nan() {
                bar.close + atr * 1.5
            } else {
                self.resistance + atr * 0.25
            };
            self.short_levels = (stop, bar.close - atr * 3.0);
        }
        let held = strategy.position();
        if held > 0.0 {
            let (stop, take) = self.long_levels;
            let exit = strategy.exit("long exit").from(UP);
            if let Some(take) = known(take) {
                exit.take(take);
            }
            if let Some(stop) = known(stop) {
                exit.stop(stop);
            }
        }
        if held < 0.0 {
            let (stop, take) = self.short_levels;
            let exit = strategy.exit("short exit").from(DOWN);
            if let Some(take) = known(take) {
                exit.take(take);
            }
            if let Some(stop) = known(stop) {
                exit.stop(stop);
            }
        }
        if held == 0.0 && self.was_held {
            self.long_levels = (f64::NAN, f64::NAN);
            self.short_levels = (f64::NAN, f64::NAN);
        }
        self.was_held = held != 0.0;
    }
}
