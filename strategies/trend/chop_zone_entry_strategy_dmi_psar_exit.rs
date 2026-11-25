// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "CHOP Zone Entry Strategy + DMI/PSAR Exit" by IronCasper, https://www.tradingview.com/script/GrP0zABg/
// The original's notice: Author:   © IronCasper
// Changed: rewritten by hand in Rust for vike.

//! Entries on the choppiness index, exits on the DMI and the parabolic SAR, 1 BTC a position. Long on the first bar the
//! index (14 bars, smoothed by SMA(4)) is at 61.8 or more with the close up on the bar, short on the first bar it is at 38.2 or
//! less with the close down, each only while the ADX is above 25 and the SAR (0.015, 0.001, 0.2) is on the trade's side: below
//! the close for a long, not below it for a short. A long closes when the SAR moves above the close, when the ADX is below 25
//! with -DI above +DI, or on a short signal, which reverses it; a short the mirror. No costs; starts with 100,000.
//!
//! The index is `100 * log10(sum of the true range over 14 bars / (highest - lowest)) / log10(14)`, the highest of each bar's
//! high or last close, whichever is higher, the lowest the mirror (vike's `Chop::with_true_extremes`). The DMI is EMA-smoothed, not RMA-smoothed: +DM, -DM and
//! the true range each through EMA(14), each DI held at its last number while it has none, and the ADX `100 * EMA(14)` of
//! `|+DI - -DI| / (+DI + -DI)`. The rule tracks which side it holds by flags, set when it signals and cleared when it closes.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Chop, Ema, Psar, Sma, TrueRange};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const LOOKBACK: usize = 14;
const ADX_KEY: f64 = 25.0;

/// The DMI as the rule writes it: EMAs where the textbook has the RMA.
// LEFT: vike's `Adx` is Wilder's (sums), this one is EMAs of the moves with the lines and ADX held; its averages are vike's.
struct Dmi {
    true_range: Ema,
    plus: Ema,
    minus: Ema,
    adx: Ema,
    plus_di: f64,
    minus_di: f64,
    adx_last: f64,
}

impl Dmi {
    fn new() -> Self {
        let ema = || Ema::new(LOOKBACK as f64).expect("a valid length").with_skip_nan(true);
        Dmi {
            true_range: ema(),
            plus: ema(),
            minus: ema(),
            adx: ema(),
            plus_di: f64::NAN,
            minus_di: f64::NAN,
            adx_last: f64::NAN,
        }
    }

    /// `[adx, +DI, -DI]` from this bar's moves and true range (all NaN on the first bar, the moves read as none).
    fn next(&mut self, up_move: f64, down_move: f64, true_range: f64) -> [f64; 3] {
        let plus = if up_move > down_move && up_move > 0.0 { up_move } else { 0.0 };
        let minus = if down_move > up_move && down_move > 0.0 { down_move } else { 0.0 };
        let range = self.true_range.next_value(true_range);
        let plus_di = 100.0 * self.plus.next_value(plus) / range;
        let minus_di = 100.0 * self.minus.next_value(minus) / range;
        if !plus_di.is_nan() {
            self.plus_di = plus_di;
        }
        if !minus_di.is_nan() {
            self.minus_di = minus_di;
        }
        let ratio = ((self.plus_di - self.minus_di) / (self.plus_di + self.minus_di)).abs();
        let adx = 100.0 * self.adx.next_value(ratio);
        if !adx.is_nan() {
            self.adx_last = adx;
        }
        [self.adx_last, self.plus_di, self.minus_di]
    }
}

struct S {
    true_range: TrueRange,
    chop: Chop,
    smooth: Sma,
    sar: Psar,
    dmi: Dmi,
    before: Option<Bar>,
    long_zone_before: bool,
    short_zone_before: bool,
    sar_up_before: Option<bool>,
    active_long: bool,
    active_short: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        true_range: TrueRange::default().with_first_bar_missing(true),
        chop: Chop::new(LOOKBACK as f64).expect("a valid length").with_true_extremes(true),
        smooth: Sma::new(4.0).expect("a valid length"),
        sar: Psar::new(0.001, 0.2)
            .and_then(|p| p.with_start(0.015))
            .expect("valid factors")
            .with_test_first(true)
            .with_direction_from_closes(true)
            .with_textbook_steps(true),
        dmi: Dmi::new(),
        before: None,
        long_zone_before: false,
        short_zone_before: false,
        sar_up_before: None,
        active_long: false,
        active_short: false,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let close_before = self.before.as_ref().map_or(f64::NAN, |b| b.close);
        let true_range = self.true_range.next(bar);
        // The index's own true range counts the first bar's high less its low.
        let raw = self.chop.next(bar);
        let chop = if raw.is_nan() { f64::NAN } else { self.smooth.next_value(raw) };
        let short_zone = chop <= 38.2 && close_before > bar.close;
        let long_zone = chop >= 61.8 && close_before < bar.close;

        let sar = self.sar.next(bar);
        let sar_up = sar < bar.close;
        let (up_move, down_move) = match &self.before {
            None => (f64::NAN, f64::NAN),
            Some(b) => (bar.high - b.high, -(bar.low - b.low)),
        };
        let [adx, plus_di, minus_di] = self.dmi.next(up_move, down_move, true_range);

        let go_long = long_zone && !self.long_zone_before && adx > ADX_KEY && sar_up;
        let go_short = short_zone && !self.short_zone_before && adx > ADX_KEY && !sar_up;
        let dmi_close_long = adx < ADX_KEY && minus_di > plus_di;
        let dmi_close_short = adx < ADX_KEY && plus_di > minus_di;
        let sar_close_long = !sar_up && self.sar_up_before == Some(true);
        let sar_close_short = sar_up && self.sar_up_before == Some(false);
        let close_long = self.active_long && (sar_close_long || dmi_close_long || go_short);
        let close_short = self.active_short && (sar_close_short || dmi_close_short || go_long);
        let reverse = (close_long && go_short) || (close_short && go_long);
        let close = !reverse && (close_long || close_short);
        let active = self.active_long || self.active_short;

        if !self.active_long && go_long && !reverse {
            strategy.entry("Long", Long).size(1.0);
            self.active_long = true;
        }
        if !self.active_short && go_short && !reverse {
            strategy.entry("Short", Short).size(1.0);
            self.active_short = true;
        }
        if active && go_long && reverse {
            strategy.entry("Long", Long).size(1.0);
            self.active_long = true;
            self.active_short = false;
        }
        if active && go_short && reverse {
            strategy.entry("Short", Short).size(1.0);
            self.active_short = true;
            self.active_long = false;
        }
        if active && close {
            strategy.close("Long");
            strategy.close("Short");
        }
        if close {
            self.active_long = false;
            self.active_short = false;
        }

        self.long_zone_before = long_zone;
        self.short_zone_before = short_zone;
        self.sar_up_before = Some(sar_up);
        self.before = Some(bar.clone());
    }
}
