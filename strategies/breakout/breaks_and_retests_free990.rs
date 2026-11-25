// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Breaks and Retests - Free990" by Free990, https://www.tradingview.com/script/800ndgbX/
// The original's notice: © Free990
// Changed: rewritten by hand in Rust for vike.

//! Support and resistance boxes from 20-bar pivots, entered on a break or on a retest. A new pivot low (a change in the last
//! one) draws a support box from the lower of the lows a bar either side of it down to the pivot; a new pivot high a resistance
//! box from the pivot down to the higher of the highs beside it. The close crossing under the support's bottom breaks it, over
//! the resistance's top breaks that, once per box; a new pivot clears its side's break. A retest is a run of bars, more than 2
//! after the break, in which the price comes back into the broken box, valid once per run when within 2 bars after it starts
//! the close is back past the run's first extreme. When flat, long 1 on the bar the resistance breaks or on a valid resistance retest,
//! short 1 on the support's. While held, a stop 2% from the entry price, or once the close has been 5% in profit, 1% from the
//! best close since. No costs; starts with 1,000,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use vike_tactics::helpers::BarsSince;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

use vike_indicators::indicators::ConfirmedPivot;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;

const LOOKBACK: usize = 20;
const RETEST_SINCE: usize = 2;
const RETEST_VALID: usize = 2;
const PROFIT: f64 = 5.0;
const GAP: f64 = 1.0;
const STOP: f64 = 2.0;

/// `ta.valuewhen(cond, src, 0)`.
struct When(f64);

impl When {
    fn step(&mut self, cond: bool, src: f64) -> f64 {
        if cond {
            self.0 = src;
        }
        self.0
    }
}

/// The rule's retest of one side: `resistance` true for the pivot-high side.
struct Retest {
    resistance: bool,
    occurred: bool,
    last_active: bool,
    first_far: When,
    value: When,
    since: BarsSince,
}

impl Retest {
    fn new(resistance: bool) -> Self {
        Retest {
            resistance,
            occurred: false,
            last_active: false,
            first_far: When(f64::NAN),
            value: When(f64::NAN),
            since: BarsSince::default(),
        }
    }

    /// `near` is the extreme on the box's side (the high for resistance), `far` the other.
    fn step(&mut self, active: bool, near: f64, far: f64, close: f64, pivot_changed: bool) -> bool {
        let event = active && !self.last_active;
        self.last_active = active;
        let first = self.first_far.step(event, far);
        let beyond = if self.resistance { far < first } else { far > first };
        let event = if beyond { active } else { event };
        let value = self.value.step(event, near);
        let since = self.since.step(event);
        // A new run may be valid again.
        if event {
            self.occurred = false;
        }
        let back = if self.resistance { close >= value } else { close <= value };
        let valid = since.is_some_and(|s| s > 0 && s <= RETEST_VALID) && back && !self.occurred;
        if valid {
            self.occurred = true;
        }
        if pivot_changed && self.occurred {
            self.occurred = false;
        }
        valid
    }
}

/// One side's box and break.
struct Side {
    last_pivot: f64,
    /// `(top, bottom)`.
    zone: Option<(f64, f64)>,
    broken: bool,
    cross: Cross,
    unbroken_since: BarsSince,
    retest: Retest,
}

impl Side {
    fn new(resistance: bool) -> Self {
        Side {
            last_pivot: f64::NAN,
            zone: None,
            broken: false,
            cross: Cross::new(),
            unbroken_since: BarsSince::default(),
            retest: Retest::new(resistance),
        }
    }
}

struct S {
    pivots: ConfirmedPivot,
    /// Highs and lows, newest first, `LOOKBACK + 2` deep.
    recent: VecDeque<(f64, f64)>,
    support: Side,
    resistance: Side,
    trailing: bool,
    best: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        pivots: ConfirmedPivot::new(LOOKBACK as f64, LOOKBACK as f64).expect("valid spans"),
        recent: VecDeque::with_capacity(LOOKBACK + 3),
        support: Side::new(false),
        resistance: Side::new(true),
        trailing: false,
        best: f64::NAN,
    })
}

/// `ta.change` of a held level: true when it moved between two known values.
fn changed(side: &mut Side, pivot: f64) -> bool {
    let moved = !side.last_pivot.is_nan() && !pivot.is_nan() && pivot != side.last_pivot;
    side.last_pivot = pivot;
    moved
}

impl S {
    fn protect(&mut self, strategy: &mut Tactics<'_>, close: f64) {
        let held = strategy.position();
        if held == 0.0 {
            self.trailing = false;
            self.best = f64::NAN;
            return;
        }
        let dir = held.signum();
        let cost = strategy.avg_cost();
        let profit = (close - cost) / cost * 100.0 * dir;
        if !self.trailing && profit >= PROFIT {
            self.trailing = true;
            self.best = close;
        }
        let stop = if self.trailing {
            self.best = if dir > 0.0 { self.best.max(close) } else { self.best.min(close) };
            self.best * (1.0 - dir * GAP / 100.0)
        } else {
            cost * (1.0 - dir * STOP / 100.0)
        };
        let (name, lots) = if dir > 0.0 { ("exit long", "buy") } else { ("exit short", "sell") };
        strategy.exit(name).from(lots).stop(stop);
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let [_, _, pivot_high, pivot_low] = self.pivots.next(bar);
        self.recent.push_front((bar.high, bar.low));
        self.recent.truncate(LOOKBACK + 2);
        let beside = |f: fn(&(f64, f64)) -> f64| {
            let at = |i: usize| self.recent.get(i).map_or(f64::NAN, f);
            (at(LOOKBACK + 1), at(LOOKBACK - 1))
        };
        let (low_before, low_after) = beside(|b| b.1);
        let (high_before, high_after) = beside(|b| b.0);
        let low_changed = changed(&mut self.support, pivot_low);
        let high_changed = changed(&mut self.resistance, pivot_high);
        if low_changed {
            let top = if low_before > low_after { low_after } else { low_before };
            self.support.zone = Some((top, pivot_low));
        }
        if high_changed {
            let bottom = if high_before > high_after { high_before } else { high_after };
            self.resistance.zone = Some((pivot_high, bottom));
        }
        let (s_top, s_bot) = self.support.zone.unwrap_or((f64::NAN, f64::NAN));
        let (r_top, r_bot) = self.resistance.zone.unwrap_or((f64::NAN, f64::NAN));

        let cu = matches!(self.support.cross.step(bar.close, s_bot), Signal::Short);
        let co = matches!(self.resistance.cross.step(bar.close, r_top), Signal::Long);
        if cu && !self.support.broken {
            self.support.broken = true;
        } else if co && !self.resistance.broken {
            self.resistance.broken = true;
        }
        if low_changed {
            self.support.broken = false;
        }
        if high_changed {
            self.resistance.broken = false;
        }

        let s_since = self.support.unbroken_since.step(!self.support.broken);
        let r_since = self.resistance.unbroken_since.step(!self.resistance.broken);
        let s_ready = s_since.is_some_and(|s| s > RETEST_SINCE);
        let r_ready = r_since.is_some_and(|s| s > RETEST_SINCE);
        let (h, l, c) = (bar.high, bar.low, bar.close);
        let s_active = s_ready
            && ((h >= s_top && c <= s_bot)
                || (h >= s_top && c >= s_bot && c <= s_top)
                || (h >= s_bot && h <= s_top));
        let r_active = r_ready
            && ((l <= r_bot && c >= r_top)
                || (l <= r_bot && c <= r_top && c >= r_bot)
                || (l <= r_top && l >= r_bot));
        let r_valid = self.resistance.retest.step(r_active, h, l, c, high_changed);
        let s_valid = self.support.retest.step(s_active, l, h, c, low_changed);

        let buy = r_since == Some(1) || r_valid;
        let sell = s_since == Some(1) || s_valid;
        let flat = strategy.position() == 0.0;
        if buy && flat {
            strategy.entry("buy", Long).size(1.0);
        }
        if sell && flat {
            strategy.entry("sell", Short).size(1.0);
        }
        self.protect(strategy, bar.close);
    }
}
