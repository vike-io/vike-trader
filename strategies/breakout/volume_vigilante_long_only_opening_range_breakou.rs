// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Volume Vigilante | Long-Only Opening Range Breakout (ORB) with Pivot Points" by VolumeVigilante, https://www.tradingview.com/script/6kDE9bLA/
// The original's notice: © VolumeVigilante
// Changed: rewritten by hand in Rust for vike.

//! A long-only opening-range breakout on the UTC day. The range is the high of the 09:30 bar, known from the 10:00 bar on. From
//! then, while flat and with no signal counted yet today, and while the bar's open is under that high, a buy stop for all of
//! the account's worth rests at it, and is cancelled after the bar that follows a signal bar (a bar that opened under the high,
//! went through it, with the day before's first pivot resistance above it) if it has not filled. Held, every bar: a stop at the
//! larger of 3% under the entry fill and a ladder that steps up through the pivot resistances and their midpoints as the high
//! reaches the next one, from the bar after the fill. At the last bar of the day the position is closed at the close, the
//! orders pulled and the range dropped. No costs; version 6: starts with 10,000, margined at 100%; the rule
//! settles orders at the close.
//!
//! The rule asks for the day before's close, high and low with lookahead and reads their `[1]`: on every bar of a day that is the
//! day before, which had closed, so nothing it reads lies ahead: the last day that ended by this bar's open, never the day in
//! progress. The days are the symbol's daily input, its history before the window included (`vike_tactics::higher::HigherTimeframeSourceCalendar`). The
//! pivot and the first three resistances are vike's `PivotPoints` of those days, the levels of the day before; the fourth and
//! fifth, which vike has not, are the rule's own from that pivot.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::PivotPoints;
use vike_indicators::strategy_kit::max;
use vike_marketdata::Bar;
use vike_model::MS_PER_DAY;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::calendar::clock;
use vike_tactics::higher::{self, HigherTimeframeSourceCalendar};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{HigherTimeframePeriod, Rule, RuleDefaults, Tactics};

const MAX_TRADES: u32 = 1;
const STOP_PERCENT: f64 = 3.0;

/// A closed day as the rule reads it: its start, its levels `[p, r1, r2, r3, s1, s2, s3]` and its `(high, low)`.
type Day = (i64, [f64; 7], (f64, f64));

const NO_DAY: Day = (i64::MIN, [f64::NAN; 7], (f64::NAN, f64::NAN));

struct S {
    days: HigherTimeframeSourceCalendar,
    pivots: PivotPoints,
    /// The last day closed, and the one before it.
    last: Day,
    before: Day,
    orb_high: f64,
    built: bool,
    in_orb: bool,
    in_position: bool,
    counter: u32,
    entry_price: f64,
    trail: f64,
    /// Whether the last bar was a signal bar the rule could have entered on.
    cancel_next: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        days: HigherTimeframeSourceCalendar::new(HigherTimeframePeriod::Day),
        pivots: PivotPoints::default(),
        last: NO_DAY,
        before: NO_DAY,
        orb_high: f64::NAN,
        built: false,
        in_orb: false,
        in_position: false,
        counter: 0,
        entry_price: f64::NAN,
        trail: 0.0,
        cancel_next: false,
    })
}

/// The price a stop entry at `level` is sized from.
fn sized_at(strategy: &Tactics<'_>, level: f64, close: f64) -> f64 {
    let level = strategy.snap_stop(Long, level);
    if level <= close { close } else { level }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn inputs(&self) -> Vec<String> {
        vec![higher::DAY.to_string()]
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        for day in self.days.next(bar, strategy.input(higher::DAY)) {
            self.pivots.next_hlc(day.high, day.low, day.close);
            // `PivotPoints` hands out the levels of the bar fed before: those of this day come with the next one fed.
            let levels = self.pivots.clone().next_hlc(f64::NAN, f64::NAN, f64::NAN);
            self.before =
                std::mem::replace(&mut self.last, (day.timestamp, levels, (day.high, day.low)));
        }
        // On a day's last bar that day has just closed: the day before is the one before it.
        let (_, levels, (previous_high, previous_low)) =
            if HigherTimeframePeriod::Day.end(self.last.0) <= bar.timestamp {
                self.last
            } else {
                self.before
            };
        let (hour, minute) = clock(bar);
        let new_day = bar.timestamp.rem_euclid(MS_PER_DAY) == 0;
        let in_orb = (hour, minute) == (9, 30);
        let orb_ended = !in_orb && self.in_orb;
        let in_position = strategy.position() > 0.0;
        let just_entered = in_position && !self.in_position;

        if new_day {
            self.orb_high = f64::NAN;
            self.built = false;
        }
        if in_orb {
            self.orb_high = if self.orb_high.is_nan() || !self.in_orb {
                bar.high
            } else {
                self.orb_high.max(bar.high)
            };
        }
        if orb_ended && !self.orb_high.is_nan() {
            self.built = true;
        }
        let ready = self.built && !self.orb_high.is_nan();
        let orb = self.orb_high;

        let [pivot, r1, r2, r3, ..] = levels;
        let r4 = pivot * 3.0 + previous_high - 3.0 * previous_low;
        let r5 = pivot * 4.0 + previous_high - 4.0 * previous_low;
        let r0_5 = orb + (r1 - orb).abs() / 2.0;
        let r1_5 = r1 + (r1 - r2).abs() / 2.0;
        let r2_5 = r2 + (r2 - r3).abs() / 2.0;
        let r3_5 = r3 + (r3 - r4).abs() / 2.0;
        let r4_5 = r4 + (r4 - r5).abs() / 2.0;

        let signal = ready && bar.open < orb && bar.high > orb && r1 > orb;
        let counted = self.counter;
        self.counter = if in_orb {
            0
        } else if signal && !in_position {
            counted + 1
        } else {
            counted
        };
        let can_enter = !in_position && counted < MAX_TRADES;

        if just_entered {
            self.entry_price = strategy.avg_cost();
        } else if !in_position {
            self.entry_price = f64::NAN;
        }
        let entry = self.entry_price;
        let initial = if in_position && !entry.is_nan() {
            (entry * (1.0 - STOP_PERCENT / 100.0)).min(entry)
        } else {
            f64::NAN
        };
        if just_entered || !in_position {
            self.trail = 0.0;
        } else {
            let previous_trail = self.trail;
            let h = bar.high;
            self.trail = if h > r5 && r4 > previous_trail {
                r4
            } else if h > r4_5 && r3_5 > previous_trail {
                r3_5
            } else if h > r4 && r3 > previous_trail {
                r3
            } else if h > r3_5 && r2_5 > previous_trail {
                r2_5
            } else if h > r3 && r2 > previous_trail {
                r2
            } else if h > r2_5 && r1_5 > previous_trail {
                r1_5
            } else if h > r2 && r1 > previous_trail {
                r1
            } else if h > r1_5 && r0_5 > previous_trail {
                r0_5
            } else if h > r0_5 && orb > previous_trail {
                orb
            } else {
                previous_trail
            };
        }
        let stop = if in_position { max(initial, self.trail) } else { f64::NAN };

        let day_end = (hour, minute) == (23, 30);
        if day_end {
            strategy.close_all().here();
            strategy.cancel("long");
            self.orb_high = f64::NAN;
            self.built = false;
            self.trail = 0.0;
            self.counter = 0;
            self.entry_price = f64::NAN;
        }

        if ready && can_enter && bar.open < self.orb_high {
            let size = share_of_worth(&sizing, 100.0, sized_at(strategy, self.orb_high, bar.close));
            strategy.entry("long", Long).size(size).stop(self.orb_high);
        }
        if self.cancel_next {
            strategy.cancel("long");
        }
        self.cancel_next = signal && can_enter;
        if in_position && !day_end && !stop.is_nan() {
            strategy.exit("SL/TP").from("long").stop(stop);
        }
        self.in_orb = in_orb;
        self.in_position = in_position;
    }
}
