// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Band-Zigzag - TrendFollower Strategy [Trendoscope]" by Trendoscope, https://www.tradingview.com/script/9FsHYtrq/
// The original's notice: © HeWhoMustNotBeNamed
// Changed: rewritten by hand in Rust for vike.

//! A zigzag whose pivots are the bars that touch a band: Bollinger bands of SMA(20) and 2 standard deviations of the close, made
//! sticky (a band keeps its value until the bar before touched one of them, a high at or over the upper or a low at or under
//! the lower) and adaptive (while the last pivot is a low the upper band may not rise, while it is a high the lower may not fall).
//! A bar whose high reaches the upper band is a pivot high, one whose low reaches the lower band a pivot low; a pivot on the
//! side of the last one replaces it when more extreme, one on the other side is added (15 are kept). A pivot is marked a higher
//! (lower) extreme when it beats the pivot two back, and carries its move as a share of the move before it and its band
//! position (percent B). The trend is up when the last pivot's move beats its band position in the direction of that pivot, else
//! down; the latest pivot may override it when its move also exceeds 1.618. On each change of that combined trend the rule
//! closes the opposite side and enters the new one, once. 10% of equity, 0.05% commission, 1000 start, a margin of 100%.
//!
//! The rule imports the Bollinger band with its sticky range from a library (HeWhoMustNotBeNamed's `ta`, MPL-2.0): the bands
//! are vike's `bollinger` and the sticky range is ported here as `Bands`; its zigzag drawing, other band kinds and stop are
//! off or only draw.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Bollinger;
use vike_indicators::strategy_kit::{max, min};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const LENGTH: f64 = 20.0;
const MULTIPLIER: f64 = 2.0;
const INVALIDATION: f64 = 1.618;
const KEPT: usize = 15;

/// One zigzag pivot: `direction` is 1 for a high and -1 for a low, doubled when it beat the pivot two back.
struct Pivot {
    price: f64,
    direction: i32,
    percent_b: f64,
    ratio: f64,
}

/// The library's sticky Bollinger bands.
struct Bands {
    bollinger: Bollinger,
    previous_high: f64,
    previous_low: f64,
    upper: f64,
    lower: f64,
}

impl Bands {
    fn next(&mut self, bar: &Bar) -> (f64, f64) {
        let [fresh_upper, _, fresh_lower] = self.bollinger.next(bar);
        // `highsource[1] >= newUpper[1]`: the bar before against the bands as they were after it.
        let breakout = self.previous_high >= self.upper || self.previous_low <= self.lower;
        let (upper, lower) = if breakout {
            (fresh_upper, fresh_lower)
        } else {
            // `nz(newUpper[1], newUpper)`: a missing earlier band gives the fresh one.
            (
                if self.upper.is_nan() { fresh_upper } else { self.upper },
                if self.lower.is_nan() { fresh_lower } else { self.lower },
            )
        };
        self.upper = upper;
        self.lower = lower;
        self.previous_high = bar.high;
        self.previous_low = bar.low;
        (upper, lower)
    }
}

struct S {
    bands: Bands,
    /// Newest first.
    zigzag: Vec<Pivot>,
    upper: f64,
    lower: f64,
    current_dir: i32,
    trend: i32,
    force: i32,
    combined: i32,
    allow_entry: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        bands: Bands {
            bollinger: Bollinger::new(LENGTH, MULTIPLIER).expect("valid parameters"),
            previous_high: f64::NAN,
            previous_low: f64::NAN,
            upper: f64::NAN,
            lower: f64::NAN,
        },
        zigzag: Vec::new(),
        upper: 0.0,
        lower: 0.0,
        current_dir: 0,
        trend: 0,
        force: 0,
        combined: 0,
        allow_entry: false,
    })
}

/// A move beats its band position: the trend sign the pivot's direction gives, or its opposite.
fn side(pivot: &Pivot, needs_more: bool) -> i32 {
    let sign = pivot.direction.signum();
    let beats = pivot.ratio > pivot.percent_b && (!needs_more || pivot.ratio > INVALIDATION);
    if beats { sign } else { -sign }
}

impl S {
    /// `unshift`: marks the pivot against the two before it, then puts it first.
    fn unshift(&mut self, mut p: Pivot) {
        if self.zigzag.len() > 1 {
            let (last, before) = (&self.zigzag[0], &self.zigzag[1]);
            let d = f64::from(p.direction);
            if p.price * d > before.price * d {
                p.direction *= 2;
            }
            p.ratio = (p.price - last.price).abs() / (before.price - last.price).abs();
        }
        self.zigzag.insert(0, p);
        self.zigzag.truncate(KEPT);
    }

    /// `addPivot`: a pivot of `direction` (1 or -1) at this bar; whether it replaced the last one.
    fn add(&mut self, bar: &Bar, direction: i32, percent_b: f64) -> bool {
        let price = if direction > 0 { bar.high } else { bar.low };
        let p = Pivot { price, direction, percent_b, ratio: 0.0 };
        let Some(last) = self.zigzag.first() else {
            self.unshift(p);
            return false;
        };
        let last_dir = last.direction.signum();
        let mut replaced = false;
        if last_dir != direction {
            self.unshift(p);
        } else if last.price * f64::from(direction) < p.price * f64::from(direction) {
            self.zigzag.remove(0);
            self.unshift(p);
            replaced = true;
        }
        replaced
    }

    fn add_pivots(&mut self, bar: &Bar, p_high: bool, p_low: bool, b_high: f64, b_low: f64) {
        if self.zigzag.is_empty() {
            if p_high {
                self.add(bar, 1, b_high);
            }
            if p_low {
                self.add(bar, -1, b_low);
            }
            return;
        }
        let last_dir = self.zigzag[0].direction.signum();
        if (p_high && last_dir == 1) || (p_low && last_dir == -1) {
            let percent_b = if last_dir > 0 { b_high } else { b_low };
            self.add(bar, last_dir, percent_b);
        }
        if (p_high && last_dir == -1) || (p_low && last_dir == 1) {
            let percent_b = if -last_dir > 0 { b_high } else { b_low };
            self.add(bar, -last_dir, percent_b);
        }
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let (upper, lower) = self.bands.next(bar);
        if self.zigzag.len() > 1 {
            let (recent, last) = (&self.zigzag[0], &self.zigzag[1]);
            self.current_dir = recent.direction;
            self.trend = side(last, false);
            self.force = side(recent, true);
        }
        self.upper = if self.current_dir >= 0 { upper } else { min(upper, self.upper) };
        self.lower = if self.current_dir <= 0 { lower } else { max(lower, self.lower) };
        let width = self.upper - self.lower;
        let b_high = (bar.high - self.lower) / width;
        let b_low = (self.upper - bar.low) / width;
        let (p_high, p_low) = (bar.high >= self.upper, bar.low <= self.lower);
        self.add_pivots(bar, p_high, p_low, b_high, b_low);

        let before = self.combined;
        self.combined = if self.trend > 0 || self.force > 0 {
            1
        } else if self.trend < 0 || self.force < 0 {
            -1
        } else {
            self.combined
        };
        let crossed = (self.combined > 0 && before <= 0) || (self.combined < 0 && before >= 0);
        if crossed {
            self.allow_entry = true;
        }
        if self.combined > 0 {
            strategy.close("Short");
            if self.allow_entry {
                self.allow_entry = false;
                strategy.entry("Long", Long).size(share_of_worth(&sizing, 10.0, bar.close));
            }
        }
        if self.combined < 0 {
            strategy.close("Long");
            if self.allow_entry {
                self.allow_entry = false;
                strategy.entry("Short", Short).size(share_of_worth(&sizing, 10.0, bar.close));
            }
        }
    }
}
