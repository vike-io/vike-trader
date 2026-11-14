// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "BSL/SSL Sweep + FVG Strategy (Fixed Entry Logic)" by jobin3632, https://www.tradingview.com/script/TQZdIvcg/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2025-09-14, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Each UTC day: the high (BSL) and low (SSL) of the bars opening 07:00 to 09:29 New York time, set at the first bar after
//! them; a sweep when a bar from 09:30 to 11:30 reaches either, tracking the extreme past it; and up to ten fair-value gaps
//! from the bars opening 07:00 to 11:30 (a bar whose low is over the high two bars before, or whose high is under the low two
//! bars before). With a gap of the first kind not yet used and the SSL swept, a bar whose high or close goes over the gap's top
//! goes long 10% of the account's worth, its stop the lowest low since the sweep and its target the BSL; the mirror goes short.
//! The oldest gap is tried first, each gap is used once, and a day takes a second trade only after its first was stopped.
//!
//! The rule tracks the trade it believes open by its own reading of each bar's high and low against the stop and the target,
//! and issues the exit only while it believes so; a new day forgets it while the position and its exit stay. No costs; starts
//! with 1,000,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_marketdata::Bar;
use vike_model::MS_PER_DAY;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::helpers::us_minute;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const SHARE: f64 = 10.0;
const GAPS: usize = 10;
const MIN_GAP: f64 = 0.001;
/// New York's standard offset from UTC, in hours.
const STANDARD: i64 = -5;

struct Gap {
    top: f64,
    bottom: f64,
    /// The rule's "bullish" gap: the bar's high under the low two bars before.
    bullish: bool,
    used: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Side {
    None,
    Long,
    Short,
}

struct S {
    day: i64,
    bars: Vec<Bar>,
    bsl: f64,
    ssl: f64,
    levels_set: bool,
    in_session_was: bool,
    bsl_swept: bool,
    ssl_swept: bool,
    sweep_high: f64,
    sweep_low: f64,
    gaps: Vec<Gap>,
    stop: f64,
    in_trade: bool,
    side: Side,
    trades_today: u32,
    first_lost: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        day: i64::MIN,
        bars: Vec::new(),
        bsl: f64::NAN,
        ssl: f64::NAN,
        levels_set: false,
        in_session_was: false,
        bsl_swept: false,
        ssl_swept: false,
        sweep_high: f64::NAN,
        sweep_low: f64::NAN,
        gaps: Vec::new(),
        stop: f64::NAN,
        in_trade: false,
        side: Side::None,
        trades_today: 0,
        first_lost: false,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let minute = us_minute(bar.timestamp, STANDARD);
        let in_session = (7 * 60..9 * 60 + 29).contains(&minute);
        let in_window = (9 * 60 + 30..11 * 60 + 30).contains(&minute);
        let in_gap_window = (7 * 60..=11 * 60 + 30).contains(&minute);
        let day = bar.timestamp.div_euclid(MS_PER_DAY);

        if day != self.day {
            self.day = day;
            (self.bsl, self.ssl, self.levels_set) = (f64::NAN, f64::NAN, false);
            (self.bsl_swept, self.ssl_swept) = (false, false);
            (self.sweep_high, self.sweep_low) = (f64::NAN, f64::NAN);
            (self.in_trade, self.side, self.trades_today, self.first_lost) =
                (false, Side::None, 0, false);
            self.gaps.clear();
        }
        if in_session {
            if self.bsl.is_nan() || bar.high > self.bsl {
                self.bsl = bar.high;
            }
            if self.ssl.is_nan() || bar.low < self.ssl {
                self.ssl = bar.low;
            }
        }
        if !in_session
            && self.in_session_was
            && !self.bsl.is_nan()
            && !self.ssl.is_nan()
            && !self.levels_set
        {
            self.levels_set = true;
        }
        self.in_session_was = in_session;

        self.bars.push(bar.clone());
        if self.bars.len() > 3 {
            self.bars.remove(0);
        }
        if let [two_back, _, _] = self.bars.as_slice() {
            let bullish =
                in_gap_window && two_back.low > bar.high && (two_back.low - bar.high) >= MIN_GAP;
            let bearish =
                in_gap_window && two_back.high < bar.low && (bar.low - two_back.high) >= MIN_GAP;
            if bullish {
                self.gaps.push(Gap { top: two_back.low, bottom: bar.high, bullish, used: false });
            } else if bearish {
                self.gaps.push(Gap { top: bar.low, bottom: two_back.high, bullish, used: false });
            }
            if self.gaps.len() > GAPS {
                self.gaps.remove(0);
            }
        }

        if in_window && self.levels_set {
            if bar.high >= self.bsl && !self.bsl_swept {
                self.bsl_swept = true;
                self.sweep_high = bar.high;
            }
            if bar.low <= self.ssl && !self.ssl_swept {
                self.ssl_swept = true;
                self.sweep_low = bar.low;
            }
        }
        if self.bsl_swept && bar.high > self.sweep_high {
            self.sweep_high = bar.high;
        }
        if self.ssl_swept && bar.low < self.sweep_low {
            self.sweep_low = bar.low;
        }

        let can_enter = self.trades_today == 0 || (self.first_lost && self.trades_today < 2);
        if can_enter && !self.in_trade && !self.gaps.is_empty() && self.levels_set {
            for gap in self.gaps.iter_mut().filter(|g| !g.used) {
                let entry = if !gap.bullish && self.ssl_swept && !self.sweep_low.is_nan() {
                    (bar.high > gap.top || bar.close > gap.top).then_some(Side::Long)
                } else if gap.bullish && self.bsl_swept && !self.sweep_high.is_nan() {
                    (bar.low < gap.bottom || bar.close < gap.bottom).then_some(Side::Short)
                } else {
                    None
                };
                let Some(side) = entry else { continue };
                if side == Side::Long {
                    strategy.entry("Long", Long).size(share_of_worth(&sizing, SHARE, bar.close));
                    self.stop = self.sweep_low;
                } else {
                    strategy.entry("Short", Short).size(share_of_worth(&sizing, SHARE, bar.close));
                    self.stop = self.sweep_high;
                }
                (self.in_trade, self.side) = (true, side);
                self.trades_today += 1;
                gap.used = true;
                break;
            }
        }

        let long = self.side == Side::Long;
        let stopped = if long { bar.low <= self.stop } else { bar.high >= self.stop };
        let reached = if long { bar.high >= self.bsl } else { bar.low <= self.ssl };
        if self.in_trade && self.trades_today == 1 && self.side != Side::None {
            if stopped {
                self.first_lost = true;
                self.in_trade = false;
            }
            if self.in_trade && reached {
                self.in_trade = false;
            }
        }
        if self.in_trade
            && self.trades_today == 2
            && self.side != Side::None
            && (stopped || reached)
        {
            self.in_trade = false;
        }

        if self.in_trade {
            match self.side {
                Side::Long => {
                    strategy.exit("Long Exit").from("Long").take(self.bsl).stop(self.stop);
                }
                Side::Short => {
                    strategy.exit("Short Exit").from("Short").take(self.ssl).stop(self.stop);
                }
                Side::None => {}
            }
        }
    }
}
