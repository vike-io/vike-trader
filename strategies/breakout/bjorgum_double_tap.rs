// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Bjorgum Double Tap" by Bjorgum, https://www.tradingview.com/script/rLkjr2sQ/
// The original's notice: © Bjorgum
// Changed: rewritten by hand in Rust for vike.

//! Double tops and bottoms from swings of 50 bars. A swing turns up on a bar whose high is the highest of the last 50 (ties
//! with an older bar included) and down on one whose low is the lowest; each turn logs the 50-bar extreme as a point, and while
//! the swing runs a new extreme moves its point. Five points are kept, the newest one still moving.
//!
//! The four before it make a double top when the first lies under the third (the neckline), the fourth is within 15% of the
//! pattern's height of the second, and the close crosses down through the neckline on this bar, once per fourth point. Its stop is
//! the higher top and its target the neckline less the height. A double bottom is the mirror. A side looks for its next pattern
//! only once the bar's low or high has reached the last one's target or stop. Short 1,000 worth on a top, long on a bottom, from
//! flat or reversing; the exit of both sides is the newest pattern's stop and target, issued on every bar. 0.04% commission and 1
//! tick of slippage; starts with 1,000.
//!
//! The rule's highest and lowest read nothing until 50 bars are in, and a value they lack counts as a turn up, so the first
//! 49 bars hold the swing up and log nothing. Its trailing stop, date filter and alert texts are inputs not taken.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Donchian;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::helpers::known;
use vike_tactics::sizing::cash_worth;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const LEN: f64 = 50.0;
const TOLERANCE: f64 = 15.0;
const FIB: f64 = 100.0;
const CASH: f64 = 1_000.0;
const POINTS: usize = 5;

/// One side's search and its active pattern: `m` is 1 for tops, -1 for bottoms.
struct Scan {
    m: f64,
    /// The rule's `_cond`: searching, as left at the end of the last bar.
    searching: bool,
    /// The active pattern's target and stop, `None` once reached.
    lines: Option<(f64, f64)>,
    stop: f64,
    limit: f64,
}

impl Scan {
    fn new(m: f64) -> Self {
        Scan { m, searching: true, lines: None, stop: f64::NAN, limit: f64::NAN }
    }

    /// One bar: whether it still searches, and the stop and target of the newest pattern.
    fn step(&mut self, points: &mut [(f64, bool)], bar: &Bar, last_close: f64) -> (bool, f64, f64) {
        let m = self.m;
        let mut found = false;
        if self.searching {
            let [y1, y2, y3] = [points[0].0, points[1].0, points[2].0];
            let (y4, traded) = points[3];
            let height = (y2 + y4) / 2.0 - y3;
            let high = y2 + height * (TOLERANCE / 100.0);
            let low = y2 - height * (TOLERANCE / 100.0);
            let target = y3 - height * (FIB / 100.0);
            let far = if y2 * m > y4 * m { y2 } else { y4 };
            if y1 * m < y3 * m
                && y4 * m <= high * m
                && y4 * m >= low * m
                && bar.close * m < y3 * m
                // The rule's `not (close[1] * m < y3 * m)`: true with no last close (y3 is a number here).
                && (last_close.is_nan() || last_close * m >= y3 * m)
                && !traded
            {
                self.lines = Some((target, far));
                points[3].1 = true;
                found = true;
            }
        }
        let enter = self.searching && found;
        let (lp, ls) = self.lines.unwrap_or((f64::NAN, f64::NAN));
        let (near, away) = if m > 0.0 { (bar.low, bar.high) } else { (bar.high, bar.low) };
        let win = near * m <= lp * m && !enter;
        let lose = away * m >= ls * m && !enter;
        let mut searching = self.searching && !found;
        if enter {
            (self.stop, self.limit) = (ls, lp);
        }
        if win || lose {
            searching = true;
            self.lines = None;
        }
        self.searching = searching;
        (searching, self.stop, self.limit)
    }
}

struct S {
    /// The last `LEN` bars' highest high and lowest low.
    range: Donchian,
    /// 1 swinging up, 0 down; `None` before the first bar.
    dir: Option<u8>,
    /// Price and whether a pattern was traded on it, oldest first; the rule starts with five empty rows.
    points: Vec<(f64, bool)>,
    tops: Scan,
    bottoms: Scan,
    /// Searching for each side on the last bar.
    last: (bool, bool),
    last_close: f64,
    stp: f64,
    lmt: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        range: Donchian::new(LEN).expect("a valid length"),
        dir: None,
        points: vec![(f64::NAN, false); POINTS],
        tops: Scan::new(1.0),
        bottoms: Scan::new(-1.0),
        last: (false, false),
        last_close: f64::NAN,
        stp: f64::NAN,
        lmt: f64::NAN,
    })
}

impl S {
    /// The swing points: a turn logs a point, an extreme in the swing's way moves the newest.
    fn log_swing(&mut self, bar: &Bar) {
        let [top, _, bottom] = self.range.next(bar);
        let full = !top.is_nan();
        // Before 50 bars the rule's offsets are missing, which it reads as "this bar is the extreme".
        let (at_high, at_low) =
            if full { (bar.high >= top, bar.low <= bottom) } else { (true, true) };
        let (piv_high, piv_low) = if full { (top, bottom) } else { (f64::NAN, f64::NAN) };
        let dir = if at_high {
            1
        } else if at_low {
            0
        } else {
            self.dir.unwrap_or(0)
        };
        let turned = self.dir.is_some_and(|d| d != dir);
        self.dir = Some(dir);
        if turned {
            self.points.push((if dir == 1 { piv_high } else { piv_low }, false));
            self.points.remove(0);
        }
        let (set_up, set_down) = (at_high && dir == 1, at_low && dir == 0);
        if set_up || set_down {
            let m = if dir == 1 { 1.0 } else { -1.0 };
            let y = if set_up { piv_high } else { piv_low };
            let newest = &mut self.points[POINTS - 1].0;
            if y * m > *newest * m {
                *newest = y;
            }
        }
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.04), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        self.log_swing(bar);
        let (bear, ss, sl) = self.tops.step(&mut self.points, bar, self.last_close);
        let (bull, ls, ll) = self.bottoms.step(&mut self.points, bar, self.last_close);
        self.last_close = bar.close;
        let (last_bear, last_bull) = std::mem::replace(&mut self.last, (bear, bull));
        let sell = last_bear && !bear;
        let buy = last_bull && !bull;
        let held = strategy.position();
        let long_entry = buy && held <= 0.0;
        let short_entry = sell && held >= 0.0;
        if short_entry {
            (self.lmt, self.stp) = (sl, ss);
        } else if long_entry {
            (self.lmt, self.stp) = (ll, ls);
        }
        if long_entry {
            strategy.entry("Long", Long).size(cash_worth(
                &sizing,
                CASH,
                sizing.slipped(Long, bar.close),
            ));
        }
        if short_entry {
            strategy.entry("Short", Short).size(cash_worth(
                &sizing,
                CASH,
                sizing.slipped(Short, bar.close),
            ));
        }
        let (cut, take) = (known(self.stp), known(self.lmt));
        for (name, lots) in [("Long Exit", "Long"), ("Short Exit", "Short")] {
            let exit = strategy.exit(name).from(lots);
            if let Some(take) = take {
                exit.take(take);
            }
            if let Some(cut) = cut {
                exit.stop(cut);
            }
        }
    }
}
