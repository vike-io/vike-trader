// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Solution Zigma" by SATI_by_OpesSemper, https://www.tradingview.com/script/k1GLeZco/
// The original's notice: © ZoomerXeus
// Ported into this file: the library "AutoFiboRetrace" (ZoomerXeus/AutoFiboRetrace/2), MPL-2.0; its notice: © ZoomerXeus
// Changed: rewritten by hand in Rust for vike.

//! An impulse line and a chandelier stop, filtered by a DMI on 15-minute bars (the rule's default, "Original 0 Extra").
//! The impulse line is the EMA(12), on the chart's bars, of the 50% retracement of the hourly range: half way between the
//! lowest low and the highest high of the last 12 hourly bars (the library's `original`). The chandelier stop (the original
//! credits Alex Orekhov's Chandelier Exit) is 3 times the 1-bar ATR below the high (long) and above the low (short), each
//! ratcheted while the close before stayed on its side; the direction turns up when the close is above the short stop as it
//! stood a bar before, down when it is below the long stop so, and holds otherwise. The DMI(14, 14) is bullish when +DI is
//! at least 25 and above the ADX, bearish the mirror.
//!
//! Flat, a free order buys 0.01 (the order size 0.001 times the leverage 10) when the close crosses up through the impulse
//! line, the DMI is bullish and the direction is up; a free order sells 0.01 when the close crosses down through it, the DMI
//! is bearish and the direction is down. A long is closed when the close is below the impulse line and the direction is
//! down, a short when the close is above it and the direction is up. Orders fill at the next open. No costs; starts with
//! 1,000,000, the language's default margin of 100%.
//!
//! The hourly bars are built from the symbol's 30-minute input from the rule's first bar
//! (`vike_tactics::higher::HigherTimeframeSourceIntraday`), each read once it has closed. The 15-minute frame is an input of
//! the run's own symbol at that bar length (decision 0271), read at the chart bar's close: the DMI over its bars from the
//! rule's first bar on, as it stands after the last of them inside the chart bar. The impulse line waits for its first
//! twelve retracements (`Ema::with_skip_nan`).
//!
//! Left out, as no order reads them: the other four entry modes and the risk-reward exit (inputs set off), the 4-hour and
//! 1-hour DMIs (the default picks the 15-minute one), the hourly trend, the regression band, the wave trend, the money flow,
//! the oscillator, the swing extremes and every plot and label.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Adx, Donchian, Ema};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::higher::{self, HigherTimeframeSourceIntraday, HigherTimeframeSourceUnseen};
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const FIFTEEN_MINUTES: &str = "15m";
/// The retracement's frame in minutes, the hourly bars its range spans, and the level read.
const RANGE_MINUTES: i64 = 60;
const RANGE_LENGTH: f64 = 12.0;
const LEVEL: f64 = 0.5;
const IMPULSE_LENGTH: f64 = 12.0;
const DMI_LENGTH: f64 = 14.0;
const DMI_KEY: f64 = 25.0;
const STOP_MULTIPLIER: f64 = 3.0;
const ORDER_SIZE: f64 = 0.001;
const LEVERAGE: f64 = 10.0;

struct S {
    /// The rule's first bar's open: no 15-minute bar before it is fed.
    from: Option<i64>,
    hours: HigherTimeframeSourceIntraday,
    range: Donchian,
    /// The retracement of the last hour closed, NaN before twelve.
    level: f64,
    impulse: Ema,
    cross: Cross,
    quarters: HigherTimeframeSourceUnseen,
    dmi: Adx,
    /// `[adx, +DI, -DI]` after the last 15-minute bar fed.
    dmi_now: [f64; 3],
    close_before: f64,
    /// The chandelier stops as the bar before left them, NaN before the first bar.
    long_stop: f64,
    short_stop: f64,
    /// 1 up, -1 down.
    direction: i8,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        from: None,
        hours: HigherTimeframeSourceIntraday::new(RANGE_MINUTES),
        range: Donchian::new(RANGE_LENGTH).expect("a valid length"),
        level: f64::NAN,
        impulse: Ema::new(IMPULSE_LENGTH).expect("a valid length").with_skip_nan(true),
        cross: Cross::new(),
        quarters: HigherTimeframeSourceUnseen::new(),
        dmi: Adx::new(DMI_LENGTH).expect("a valid length").with_held_lines(true),
        dmi_now: [f64::NAN; 3],
        close_before: f64::NAN,
        long_stop: f64::NAN,
        short_stop: f64::NAN,
        direction: 1,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn inputs(&self) -> Vec<String> {
        vec![higher::HALF_HOUR.to_string(), FIFTEEN_MINUTES.to_string()]
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let from = *self.from.get_or_insert(bar.timestamp);
        for hour in self.hours.next(bar, strategy.input(higher::HALF_HOUR)) {
            let [high, _, low] = self.range.next(&hour);
            self.level = (high - low) * LEVEL + low;
        }
        let impulse = self.impulse.next_value(self.level);
        let quarters = self.quarters.next(strategy.input(FIFTEEN_MINUTES));
        for quarter in &quarters[quarters.partition_point(|b| b.timestamp < from)..] {
            self.dmi_now = self.dmi.next(quarter);
        }
        let [adx, plus, minus] = self.dmi_now;

        // The chandelier stop: the 1-bar ATR is the true range, the first bar's its high less its low.
        let close = bar.close;
        let before = self.close_before;
        let true_range = if before.is_nan() {
            bar.high - bar.low
        } else {
            (bar.high - bar.low).max((bar.high - before).abs()).max((bar.low - before).abs())
        };
        let atr = STOP_MULTIPLIER * true_range;
        let mut long_stop = bar.high - atr;
        let long_stop_before = if self.long_stop.is_nan() { long_stop } else { self.long_stop };
        if before > long_stop_before {
            long_stop = long_stop.max(long_stop_before);
        }
        let mut short_stop = bar.low + atr;
        let short_stop_before = if self.short_stop.is_nan() { short_stop } else { self.short_stop };
        if before < short_stop_before {
            short_stop = short_stop.min(short_stop_before);
        }
        if close > short_stop_before {
            self.direction = 1;
        } else if close < long_stop_before {
            self.direction = -1;
        }
        (self.long_stop, self.short_stop, self.close_before) = (long_stop, short_stop, close);
        let up = self.direction == 1;

        let cross = self.cross.step(close, impulse);
        if strategy.position() == 0.0 {
            if matches!(cross, Signal::Long) && plus >= DMI_KEY && plus > adx && up {
                strategy.order("Long", Long).size(ORDER_SIZE * LEVERAGE);
            }
            if matches!(cross, Signal::Short) && minus >= DMI_KEY && minus > adx && !up {
                strategy.order("Short", Short).size(ORDER_SIZE * LEVERAGE);
            }
        }
        if close < impulse && !up {
            strategy.close("Long");
        }
        if close > impulse && up {
            strategy.close("Short");
        }
    }
}
