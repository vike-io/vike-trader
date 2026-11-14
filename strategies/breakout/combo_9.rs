// SPDX-License-Identifier: Apache-2.0 AND MPL-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-vcp-integration-01/strategy.pine
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived in part from "Vortex Confluence Protocol [JOAT]", © officialjackofalltrades, https://www.tradingview.com/script/XtP27UK6-Vortex-Confluence-Protocol-JOAT/
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long inside the 08:00 to 16:00 New York session when all hold: the close is above the last confirmed pivot high (5 bars each
//! side), the bar touches a bull fair value gap zone, the bar is an up bar with volume over 2 deviations from its SMA(20) or the
//! 10-bar volume delta is above zero, and ADX(14) is above 25 with +DI over -DI. Short is the mirror. A setup reverses what is
//! held, and the first bar after the session flattens. The rule also computes a smoothed RSI that decides nothing, left out here.
//!
//! The `adx` row reads NaN while it warms up where this rule reads 0, which a long warm-up forgets. The zone list does remember
//! its start.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use std::collections::VecDeque;
use vike_indicators::indicators::Atr;
use vike_indicators::indicators::{ConfirmedPivot, Sma, Stddev};
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::helpers::us_minute;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

const SESSION_OPENS: i64 = 8 * 60;
const SESSION_CLOSES: i64 = 16 * 60;
const VOLUME_DEVIATIONS: f64 = 2.0;
const DELTA_WINDOW: usize = 10;
/// Added to the bar's range so that a bar with no range does not divide by zero.
const RANGE_PAD: f64 = 0.0001;
const TREND_ADX: f64 = 25.0;

/// Buying volume less selling volume. An up bar counts all its volume as bought and a down bar as sold; the other side, and both
/// sides of a doji, are the share the close's place in the range gives. Vike's `net_volume` signs the whole bar.
fn volume_delta(bar: &Bar) -> f64 {
    let range = bar.high - bar.low + RANGE_PAD;
    let bought =
        if bar.close > bar.open { bar.volume } else { bar.volume * (bar.close - bar.low) / range };
    let sold =
        if bar.close < bar.open { bar.volume } else { bar.volume * (bar.high - bar.close) / range };
    bought - sold
}

struct S {
    pivots: ConfirmedPivot,
    last_pivot_high: f64,
    last_pivot_low: f64,
    zones: GapZones,
    volume_mean: Sma,
    volume_deviation: Stddev,
    // Oldest first, and NaN until full, so the sum is NaN until then.
    deltas: [f64; DELTA_WINDOW],
    adx: IndicatorLine,
    was_in_session: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        pivots: ConfirmedPivot::new(5.0, 5.0).expect("valid spans"),
        last_pivot_high: f64::NAN,
        last_pivot_low: f64::NAN,
        zones: GapZones::new(),
        volume_mean: Sma::new(20.0).expect("a valid period"),
        volume_deviation: Stddev::new(20.0).expect("a valid period"),
        deltas: [f64::NAN; DELTA_WINDOW],
        adx: IndicatorLine::new("adx", &[14.0]),
        was_in_session: false,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let [_, _, newest_high, newest_low] = self.pivots.next(bar);
        self.last_pivot_high = newest_high;
        self.last_pivot_low = newest_low;
        let (in_bull_zone, in_bear_zone) = self.zones.step(bar);

        let volume_mean = self.volume_mean.next_value(bar.volume);
        let deviation = self.volume_deviation.next_value(bar.volume);
        let z_score = if deviation > 0.0 { (bar.volume - volume_mean) / deviation } else { 0.0 };
        let anomaly = z_score.abs() > VOLUME_DEVIATIONS;

        self.deltas.rotate_left(1);
        self.deltas[DELTA_WINDOW - 1] = volume_delta(bar);
        let delta_sum: f64 = self.deltas.iter().sum();

        // The `adx` row's lines are adx, +DI, -DI.
        let lines = self.adx.next(bar);
        let (plus_di, minus_di) = (lines[1], lines[2]);
        let in_session = (SESSION_OPENS..SESSION_CLOSES).contains(&us_minute(bar.timestamp, -5));
        let session_ended = !in_session && self.was_in_session;
        self.was_in_session = in_session;

        // A setup needs the session and opposite DI readings, so one bar asks for one thing at most.
        let trending = lines[0] > TREND_ADX && in_session;
        if bar.close > self.last_pivot_high
            && in_bull_zone
            && ((anomaly && bar.close > bar.open) || delta_sum > 0.0)
            && trending
            && plus_di > minus_di
        {
            Signal::Long
        } else if bar.close < self.last_pivot_low
            && in_bear_zone
            && ((anomaly && bar.close < bar.open) || delta_sum < 0.0)
            && trending
            && minus_di > plus_di
        {
            Signal::Short
        } else if session_ended {
            Signal::Close
        } else {
            Signal::Hold
        }
    }
}

const MIN_GAP_IN_ATRS: f64 = 0.3;
const MAX_ZONES: usize = 30;

struct Zone {
    top: f64,
    bottom: f64,
    bullish: bool,
}

/// The active zones, copied from `fvg_active_zones`.
struct GapZones {
    atr: Atr,
    zones: VecDeque<Zone>,
    // Newest first. A missing bar is a NaN, which compares false.
    highs: [f64; 2],
    lows: [f64; 2],
    last_up: bool,
    last_down: bool,
}

impl GapZones {
    fn new() -> Self {
        GapZones {
            atr: Atr::new(14.0).expect("a valid period"),
            zones: VecDeque::new(),
            highs: [f64::NAN; 2],
            lows: [f64::NAN; 2],
            last_up: false,
            last_down: false,
        }
    }

    /// Records the zone this bar makes, if any, then says whether the bar touches a bull zone and a bear zone.
    fn step(&mut self, bar: &Bar) -> (bool, bool) {
        let min_gap = self.atr.next(bar) * MIN_GAP_IN_ATRS;
        let (high_2, low_2) = (self.highs[1], self.lows[1]);
        // A low above an old high and a high below an old low cannot both hold, so a bar makes one zone at most.
        if bar.low > high_2 && self.last_up && bar.low - high_2 >= min_gap {
            self.zones.push_back(Zone { top: bar.low, bottom: high_2, bullish: true });
        } else if bar.high < low_2 && self.last_down && low_2 - bar.high >= min_gap {
            self.zones.push_back(Zone { top: low_2, bottom: bar.high, bullish: false });
        }
        if self.zones.len() > MAX_ZONES {
            self.zones.pop_front();
        }
        self.highs = [bar.high, self.highs[0]];
        self.lows = [bar.low, self.lows[0]];
        (self.last_up, self.last_down) = (bar.close > bar.open, bar.close < bar.open);

        let touches = |bullish: bool| {
            self.zones.iter().any(|zone| {
                zone.bullish == bullish && bar.low <= zone.top && bar.high >= zone.bottom
            })
        };
        (touches(true), touches(false))
    }
}
