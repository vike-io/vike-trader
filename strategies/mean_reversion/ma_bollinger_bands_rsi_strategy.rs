// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "MA Bollinger Bands + RSI (Strategy)" by LucasVivien, https://www.tradingview.com/script/2MMNHl97/
// The original's notice: Scipt Author: © LucasVivien
// Changed: rewritten by hand in Rust for vike.

//! A close back inside a Bollinger band after its open lay outside, with an RSI cross of its neutral line in the last 11 bars,
//! outside extreme volatility. Long a fixed 10,000 of cash (`10000 / close`) on a bull signal, short on a bear one, each
//! reversing the other; one entry per change of direction, and another after a stop-out (a flat book re-enters on a signal
//! unless the stop was hit on that very bar). A trailing stop ratchets 9 ATR beyond the last two bars' low or high. Orders fill
//! at the next open. 0.1% commission; starts with 10,000.
//!
//! The bands are SMA(200) of the close plus and minus twice its 200-bar deviation. A bull signal is an open under the lower
//! band with a close back over it, a bear one the mirror at the upper band; the RSI is 6 bars and its neutral line 50, its
//! cross counted from this bar back 10 more. Volatility is the band width against its 2000-bar mean: wider than 1.5 times or
//! narrower than half of it blocks the signal; that mean is missing for the first 2198 bars, which blocks nothing. A
//! direction change is the first signal of the other side. The trailing stop starts at the signal bar's value, only ever
//! moves toward the price, and is cleared when the book goes flat; the rule's stop-loss and take-profit are off.
//!
//! The rule recalculates after every fill of a bar (`calc_on_order_fills`) on that bar's final values, so each fill runs
//! it again at that point (`Rule::after_fill`): a stop that fills mid-bar can be followed by the signal of that same bar, and
//! the stop of an entry that fills at the open is live from the open, worked out with the whole bar's low.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use vike_indicators::strategy_kit::{max, min};
use vike_tactics::helpers::known;
use vike_tactics::sizing::cash_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

use vike_indicators::indicators::{Atr, Bollinger, Rsi};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;

const BAND_LENGTH: usize = 200;
const BAND_MULTIPLIER: f64 = 2.0;
const RSI_LENGTH: f64 = 6.0;
const RSI_NEUTRAL: f64 = 50.0;
const RSI_LOOKBACK: usize = 10;
const BASE_LENGTH: usize = 2000;
const VOL_FACTOR: f64 = 5.0;
const ATR_LENGTH: f64 = 14.0;
const ATR_MULTIPLIER: f64 = 9.0;
const CASH: f64 = 10_000.0;

/// What one bar says, from its own final values and what came before it.
struct Signals {
    valid_long: bool,
    valid_short: bool,
    long_trail: f64,
    short_trail: f64,
    rsi: Rsi,
    rsi_value: f64,
    cross: (bool, bool),
    band_width: f64,
}

struct S {
    bands: Bollinger,
    widths: Vec<f64>,
    rsi: Rsi,
    rsi_value: f64,
    /// The RSI crosses (up, down) of the bars before, newest first.
    crosses: VecDeque<(bool, bool)>,
    atr: Atr,
    last_low: f64,
    last_high: f64,
    /// The rule's variables as the bar before left them.
    last_long: bool,
    last_short: bool,
    trail: f64,
    /// `strategy.position_size[1]`: the position at the last bar's close.
    held_before: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        bands: Bollinger::new(BAND_LENGTH as f64, BAND_MULTIPLIER).expect("valid parameters"),
        widths: Vec::new(),
        rsi: Rsi::new(RSI_LENGTH).expect("a valid length").with_rs_form(true),
        rsi_value: f64::NAN,
        crosses: VecDeque::new(),
        atr: Atr::new(ATR_LENGTH).expect("a valid length"),
        last_low: f64::NAN,
        last_high: f64::NAN,
        last_long: false,
        last_short: false,
        trail: f64::NAN,
        held_before: 0.0,
    })
}

impl S {
    /// The bar's signals, worked out from the bars before it and its own final values, keeping nothing.
    fn signals(&self, bar: &Bar) -> Signals {
        // vike's bands, stepped on a copy as the RSI is: a rerun after a fill keeps nothing.
        let [upper, _, lower] = self.bands.clone().next(bar);
        let width = upper - lower;
        let bull_bar = bar.open < lower && bar.close > lower;
        let bear_bar = bar.open > upper && bar.close < upper;

        // The RSI is the reference's (`Rsi::with_rs_form`), stepped on a copy: a rerun after a fill keeps nothing.
        let mut rsi = self.rsi.clone();
        let rsi_value = rsi.next_value(bar.close);
        let cross = (
            rsi_value > RSI_NEUTRAL && self.rsi_value <= RSI_NEUTRAL,
            rsi_value < RSI_NEUTRAL && self.rsi_value >= RSI_NEUTRAL,
        );
        let recent = || self.crosses.iter().take(RSI_LOOKBACK);
        let rsi_bull = cross.0 || recent().any(|c| c.0);
        let rsi_bear = cross.1 || recent().any(|c| c.1);

        // Left: the 2000-width mean, longer than `Sma`'s registry range (1..=1000) lets it be.
        let base = if self.widths.len() + 1 >= BASE_LENGTH && !width.is_nan() {
            let earlier = &self.widths[self.widths.len() + 1 - BASE_LENGTH..];
            if earlier.iter().any(|w| w.is_nan()) {
                f64::NAN
            } else {
                (earlier.iter().sum::<f64>() + width) / BASE_LENGTH as f64
            }
        } else {
            f64::NAN
        };
        let high_vol = width > base + base * VOL_FACTOR / 10.0;
        let low_vol = width < base - base * VOL_FACTOR / 10.0;

        let atr = self.atr.clone().next(bar);
        let reach = atr * ATR_MULTIPLIER;
        // `ta.lowest(low, 2)` and `ta.highest(high, 2)` over this bar and the one before, missing on the first bar.
        let long_trail = min(self.last_low, bar.low) - reach;
        let short_trail = max(self.last_high, bar.high) + reach;
        Signals {
            valid_long: bull_bar && rsi_bull && !high_vol && !low_vol,
            valid_short: bear_bar && rsi_bear && !high_vol && !low_vol,
            long_trail,
            short_trail,
            rsi,
            rsi_value,
            cross,
            band_width: width,
        }
    }

    /// One run of the rule on this bar: its orders, and the variables it leaves, `(last_long, last_short, trail)`.
    fn run(&self, strategy: &mut Tactics<'_>, bar: &Bar, s: &Signals) -> (bool, bool, f64) {
        let sizing = strategy.sizing();
        let held = strategy.position();
        let before = self.held_before;
        let (mut last_long, mut last_short) = (self.last_long, self.last_short);
        if s.valid_long {
            (last_long, last_short) = (true, false);
        }
        if s.valid_short {
            (last_long, last_short) = (false, true);
        }
        let new_direction = (last_long && !self.last_long) || (last_short && !self.last_short);

        let fresh = (s.valid_long && before <= 0.0)
            || (s.valid_short && before >= 0.0)
            || ((s.valid_long || s.valid_short) && before == 0.0);
        let mut trail = self.trail;
        if fresh {
            trail = if s.valid_long {
                s.long_trail
            } else if s.valid_short {
                s.short_trail
            } else {
                f64::NAN
            };
        }
        if held > 0.0 {
            trail = max(max(s.long_trail, trail), self.trail);
        }
        if held < 0.0 {
            trail = min(min(s.short_trail, trail), self.trail);
        }
        if before != 0.0 && held == 0.0 {
            trail = f64::NAN;
        }

        // Whether the last bar's trailing stop was reached on a position held at its close.
        let long_hit = before > 0.0 && bar.low <= self.trail;
        let short_hit = before < 0.0 && bar.high >= self.trail;
        let size = cash_worth(&sizing, CASH, bar.close);
        if (s.valid_long && new_direction) || (s.valid_long && before == 0.0 && !long_hit) {
            strategy.entry("Long", Long).size(size);
        }
        if held > 0.0 {
            let exit = strategy.exit("Long exit").from("Long");
            if let Some(cut) = known(trail) {
                exit.stop(cut);
            }
        }
        if (s.valid_short && new_direction) || (s.valid_short && before == 0.0 && !short_hit) {
            strategy.entry("Short", Short).size(size);
        }
        if held < 0.0 {
            let exit = strategy.exit("Short exit").from("Short");
            if let Some(cut) = known(trail) {
                exit.stop(cut);
            }
        }
        (last_long, last_short, trail)
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.1), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let s = self.signals(bar);
        (self.last_long, self.last_short, self.trail) = self.run(strategy, bar, &s);
        self.held_before = strategy.position();
        self.bands.next(bar);
        self.widths.push(s.band_width);
        self.rsi = s.rsi;
        self.rsi_value = s.rsi_value;
        self.crosses.push_front(s.cross);
        self.crosses.truncate(RSI_LOOKBACK);
        self.atr.next(bar);
        (self.last_low, self.last_high) = (bar.low, bar.high);
    }

    fn after_fill(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let s = self.signals(bar);
        self.run(strategy, bar, &s);
    }
}
