// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Quasimodo Pattern Strategy Back Test [TradingFinder] QM Trading" by TradingFinder, https://www.tradingview.com/script/7WpyJVvp/
// The original's notice: © chartbox90
// Changed: rewritten by hand in Rust for vike.

//! Swing points from pivots of 5 bars a side, kept as a list labelled H/HH/LH and L/HL/LL by the reference's own rules
//! (a new pivot may replace the last point instead of joining it). A bearish pattern is the list ending HH, HL, HH, LL with
//! the point before them under the last LL, found on the bar that confirms that LL; a bullish one is the mirror (LL, LH, LL,
//! HH, the point before above the last HH).
//!
//! After a bearish pattern, short 10,000 worth once when the high reaches the first HH and stays under the second, while
//! fewer bars than the distance between the two HHs have passed since the LL. The short is closed when the low reaches the
//! HL, or else when the high reaches the second HH. The bullish side is the mirror. A pattern's levels stay in force until
//! the next pattern of its side. No costs; starts with 1,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::ConfirmedPivot;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::cash_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const SPAN: i64 = 5;
const CASH: f64 = 10_000.0;
/// How long, in multiples of half the pattern's span, its entry stays valid.
const VALIDITY: f64 = 2.0;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    H,
    Hh,
    Lh,
    L,
    Hl,
    Ll,
}

#[derive(Clone, Copy)]
struct Point {
    kind: Kind,
    value: f64,
    index: i64,
}

/// One side's pattern: its levels, where it was found, and whether its entry was taken.
struct Pattern {
    /// The detector's latch: the pattern's start, and whether it was found since that start last changed.
    start: f64,
    found: bool,
    index: f64,
    entry: f64,
    stop: f64,
    target: f64,
    /// The entry level on the bar before.
    last_entry: f64,
    taken: bool,
}

impl Pattern {
    fn new() -> Self {
        Pattern {
            start: 0.0,
            found: false,
            index: 0.0,
            entry: f64::NAN,
            stop: f64::NAN,
            target: f64::NAN,
            last_entry: f64::NAN,
            taken: false,
        }
    }
}

struct S {
    pivots: ConfirmedPivot,
    bar: i64,
    /// The newest pivot of each side: value and bar index.
    high: (f64, i64),
    low: (f64, i64),
    points: Vec<Point>,
    /// The last five points as the detector last read them, newest first.
    seen: [Point; 5],
    bear: Pattern,
    bull: Pattern,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let none = Point { kind: Kind::H, value: 0.0, index: 0 };
    Box::new(S {
        pivots: ConfirmedPivot::new(SPAN as f64, SPAN as f64).expect("valid spans"),
        bar: -1,
        high: (f64::NAN, 0),
        low: (f64::NAN, 0),
        points: Vec::new(),
        seen: [none; 5],
        bear: Pattern::new(),
        bull: Pattern::new(),
    })
}

fn is_high(kind: Kind) -> bool {
    matches!(kind, Kind::H | Kind::Hh | Kind::Lh)
}

impl S {
    fn last(&self) -> Point {
        *self.points.last().expect("a point")
    }

    /// The label a new high gets: against the point two back once there are more than two.
    fn push_high(&mut self) {
        let n = self.points.len();
        let kind = if n > 2 {
            if self.points[n - 2].value < self.high.0 { Kind::Hh } else { Kind::Lh }
        } else {
            Kind::H
        };
        self.points.push(Point { kind, value: self.high.0, index: self.high.1 });
    }

    fn push_low(&mut self) {
        let n = self.points.len();
        let kind = if n > 2 {
            if self.points[n - 2].value < self.low.0 { Kind::Hl } else { Kind::Ll }
        } else {
            Kind::L
        };
        self.points.push(Point { kind, value: self.low.0, index: self.low.1 });
    }

    fn replace_high(&mut self) {
        self.points.pop();
        self.push_high();
    }

    fn replace_low(&mut self) {
        self.points.pop();
        self.push_low();
    }

    /// The list's rules for the pivots this bar confirms, `NaN` for none.
    fn update(&mut self, high: f64, low: f64, close: f64) {
        let (is_h, is_l) = (!high.is_nan(), !low.is_nan());
        if self.points.is_empty() {
            // Both on the first pivot bar starts nothing.
            if is_h && !is_l {
                self.points.push(Point { kind: Kind::H, value: self.high.0, index: self.high.1 });
            } else if is_l && !is_h {
                self.points.push(Point { kind: Kind::L, value: self.low.0, index: self.low.1 });
            }
            return;
        }
        let last = self.last();
        let v = last.value;
        if is_h && is_l {
            match last.kind {
                Kind::L | Kind::Ll => {
                    if low < v {
                        self.replace_low()
                    } else {
                        self.push_high()
                    }
                }
                Kind::H | Kind::Hh => {
                    if high > v {
                        self.replace_high()
                    } else {
                        self.push_low()
                    }
                }
                Kind::Lh => {
                    if high < v {
                        self.push_low();
                    } else if high > v {
                        if close < v {
                            self.replace_high();
                        } else if close > v {
                            self.push_low();
                        }
                    }
                }
                Kind::Hl => {
                    if low > v {
                        self.push_high();
                    } else if low < v {
                        if close > v {
                            self.replace_low();
                        } else if close < v {
                            self.push_high();
                        }
                    }
                }
            }
        } else if is_h {
            if is_high(last.kind) {
                if v < self.high.0 {
                    self.replace_high();
                }
            } else if high > v {
                self.push_high();
            } else if high < v {
                self.replace_low();
            }
        } else if is_l {
            if is_high(last.kind) {
                if low < v {
                    self.push_low();
                } else if low > v {
                    self.replace_high();
                }
            } else if v > self.low.0 {
                self.replace_low();
            }
        }
    }
}

/// Finds a side's pattern on this bar: the latch takes it once, until the pattern's start changes.
fn detect(p: &mut Pattern, wanted: [Kind; 4], seen: &[Point; 5], beyond: bool, bar: i64) {
    let kinds = [seen[0].kind, seen[1].kind, seen[2].kind, seen[3].kind];
    if kinds == wanted && beyond && seen[0].index == bar - SPAN && !p.found {
        p.found = true;
        p.start = seen[1].value;
        p.index = seen[0].index as f64;
        p.entry = seen[3].value;
        p.stop = seen[1].value;
        p.target = seen[2].value;
    }
    if p.start != seen[1].value {
        p.found = false;
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        self.bar += 1;
        let [high, low, _, _] = self.pivots.next(bar);
        if !high.is_nan() {
            self.high = (high, self.bar - SPAN);
        }
        if !low.is_nan() {
            self.low = (low, self.bar - SPAN);
        }
        self.update(high, low, bar.close);

        let n = self.points.len();
        if n > 5 {
            for (k, slot) in self.seen.iter_mut().enumerate() {
                *slot = self.points[n - 1 - k];
            }
        }
        let seen = self.seen;
        // Half the span between the pattern's two outer swings, read off the list as it is now.
        let half = (seen[1].index - seen[3].index) as f64 / 2.0;
        let bar_ix = self.bar;
        let bear_beyond = seen[4].value < seen[0].value;
        detect(
            &mut self.bear,
            [Kind::Ll, Kind::Hh, Kind::Hl, Kind::Hh],
            &seen,
            bear_beyond,
            bar_ix,
        );
        let bull_beyond = seen[4].value > seen[0].value;
        detect(
            &mut self.bull,
            [Kind::Hh, Kind::Ll, Kind::Lh, Kind::Ll],
            &seen,
            bull_beyond,
            bar_ix,
        );

        let now = bar_ix as f64;
        let bear = &mut self.bear;
        if bar.high >= bear.entry
            && bar.high < bear.stop
            && !bear.taken
            && bear.index + half * VALIDITY > now
        {
            bear.taken = true;
            strategy.entry("sell", Short).size(cash_worth(&sizing, CASH, bar.close));
        }
        if bar.low <= bear.target || bar.high >= bear.stop {
            strategy.close("sell");
        }
        if bear.entry != bear.last_entry {
            bear.taken = false;
        }
        bear.last_entry = bear.entry;

        let bull = &mut self.bull;
        if bar.low <= bull.entry
            && bar.low > bull.stop
            && !bull.taken
            && bull.index + half * VALIDITY > now
        {
            strategy.entry("buy", Long).size(cash_worth(&sizing, CASH, bar.close));
            bull.taken = true;
        }
        if bar.high >= bull.target || bar.low <= bull.stop {
            strategy.close("buy");
        }
        if bull.entry != bull.last_entry {
            bull.taken = false;
        }
        bull.last_entry = bull.entry;
    }
}
