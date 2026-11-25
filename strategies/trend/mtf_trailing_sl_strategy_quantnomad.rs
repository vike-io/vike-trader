// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "MTF Trailing SL Strategy [QuantNomad]" by QuantNomad, https://www.tradingview.com/script/pVLm0yC5/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2020-03-24, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! An ATR trailing stop is worked out on the chart and on the 2-hour, 3-hour and 4-hour bars; each gives a side (long while
//! the high has reached the stop, short while the low has fallen to it, the last one kept between). Long all of the
//! account's worth when all four sides are long, short when all four are short; a long is closed when the four sides add to
//! zero or less, a short when they add to zero or more. Orders fill at the next open. No costs; starts with 100,000.
//!
//! The stop is the rule's own (`Tsl`): twice the 14-bar ATR beyond the bar's low (long) or high (short), kept while the side
//! holds and ratcheted toward the price. The higher frames read it with lookahead on and one bar back, which is the stop of
//! the last higher bar completed before the bar in progress, whole through that bar's period; a 30-minute bar that ends the
//! period still sees the one before (`Frame`). The rule looks ahead only in name: a value one higher bar back is complete.
//!
//! The higher bars are built from the symbol's 30-minute input from the rule's first bar (`vike_tactics::higher::HigherTimeframeSourceIntraday`). The
//! rule's date range (2016 to 2100) holds on every bar.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Atr;
use vike_indicators::strategy_kit::{max, min};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::helpers::nan_to_zero;
use vike_tactics::higher::{self, HigherTimeframeSourceIntraday};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{HigherTimeframePeriod, Rule, RuleDefaults, Tactics, TacticsInput};

const ATR_LENGTH: f64 = 14.0;
const ATR_MULTIPLIER: f64 = 2.0;

/// The rule's `tsl()`, run bar by bar on any series of bars.
struct Tsl {
    atr: Atr,
    pos: f64,
    stop: f64,
}

impl Tsl {
    fn new() -> Self {
        Tsl { atr: Atr::new(ATR_LENGTH).expect("a valid length"), pos: 0.0, stop: f64::NAN }
    }

    /// The stop after this bar.
    fn next(&mut self, bar: &Bar) -> f64 {
        let sl_val = ATR_MULTIPLIER * self.atr.next(bar);
        let (pos, was) = (nan_to_zero(self.pos), nan_to_zero(self.stop));
        let long = pos != 1.0 && bar.high > was;
        let short = pos != -1.0 && bar.low < was;
        let stop = if short {
            bar.high + sl_val
        } else if long {
            bar.low - sl_val
        } else if pos == 1.0 {
            max(bar.low - sl_val, was)
        } else if pos == -1.0 {
            min(bar.high + sl_val, was)
        } else {
            was
        };
        self.pos = if long {
            1.0
        } else if short {
            -1.0
        } else {
            pos
        };
        self.stop = stop;
        stop
    }
}

/// One higher timeframe's stop as the chart sees it with lookahead on and one bar back: the stop of the last higher bar
/// completed before the period of the chart bar.
struct Frame {
    every: HigherTimeframePeriod,
    bars: HigherTimeframeSourceIntraday,
    tsl: Tsl,
    /// `(higher bar's start, its stop)` of the last higher bar closed, and of the one before it.
    last: (i64, f64),
    before: (i64, f64),
}

impl Frame {
    fn new(minutes: i64) -> Self {
        let none = (i64::MIN, f64::NAN);
        let (every, bars) =
            (HigherTimeframePeriod::Minutes(minutes), HigherTimeframeSourceIntraday::new(minutes));
        Frame { every, bars, tsl: Tsl::new(), last: none, before: none }
    }

    /// Takes one chart bar and gives the stop it reads, `tsl()[1]` of the higher bar in progress: the stop of the last
    /// higher bar that ended by this bar's open (on the bar that ends one, the one before it).
    fn next(&mut self, bar: &Bar, half_hour: TacticsInput<'_>) -> f64 {
        for higher in self.bars.next(bar, half_hour) {
            let stop = self.tsl.next(&higher);
            self.before = std::mem::replace(&mut self.last, (higher.timestamp, stop));
        }
        if self.every.end(self.last.0) <= bar.timestamp { self.last.1 } else { self.before.1 }
    }
}

struct S {
    chart: Tsl,
    frames: [Frame; 3],
    /// The chart's side and each frame's, as the rule's `pos1` to `pos4` keep them.
    sides: [f64; 4],
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        chart: Tsl::new(),
        frames: [Frame::new(120), Frame::new(180), Frame::new(240)],
        sides: [0.0; 4],
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn inputs(&self) -> Vec<String> {
        vec![higher::HALF_HOUR.to_string()]
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let half_hour = strategy.input(higher::HALF_HOUR);
        let stops = [
            self.chart.next(bar),
            self.frames[0].next(bar, half_hour),
            self.frames[1].next(bar, half_hour),
            self.frames[2].next(bar, half_hour),
        ];
        for (side, stop) in self.sides.iter_mut().zip(stops) {
            *side = if bar.low <= stop {
                -1.0
            } else if bar.high >= stop {
                1.0
            } else {
                *side
            };
        }
        let total: f64 = self.sides.iter().sum();
        if total == 4.0 {
            strategy.entry("long", Long).size(share_of_worth(&sizing, 100.0, bar.close));
        } else if total <= 0.0 {
            strategy.close("long");
        }
        if total == -4.0 {
            strategy.entry("short", Short).size(share_of_worth(&sizing, 100.0, bar.close));
        } else if total >= 0.0 {
            strategy.close("short");
        }
    }
}
