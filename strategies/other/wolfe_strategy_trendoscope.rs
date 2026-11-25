// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Wolfe Strategy [Trendoscope]" by Trendoscope, https://www.tradingview.com/script/EGvQ2nLb/
// The original's notice: © HeWhoMustNotBeNamed
// Changed: rewritten by hand in Rust for vike.

//! Wolfe wedges read off a zigzag, traded long only. The zigzag keeps up to 250 pivots: a pivot high when the bar's high is the
//! highest of the last 8 bars, a pivot low likewise, a new extreme replacing the last pivot on its side. On each new pivot the
//! last five pivots (the newest a low) are a long wedge when the second is the highest and the fifth the lowest of the five, the
//! first lies between the third and the fourth, and the lines through pivots 1 and 5 and through 2 and 4 contract toward each
//! other and meet within twice their width. Its entry is the line through 2 and 4 at the next bar, its stop where the two lines
//! meet, its target the line through 1 and 4 at that bar; a pattern sharing three pivots with one of the last 10 is not new, and
//! one whose reward to risk is under 1 places no order. A limit entry when the close is already past the entry, else a stop
//! entry, 10% of equity, with a stop and a target, and the entry is moved to the line every bar while the price has not reached
//! it; a pattern is dropped when its target is reached, when it closes under its stop after reaching the entry, or when the
//! line's end has passed unreached. 0.05% commission, 1000 start, up to 10 entries, a margin of 100%.
//!
//! The rule imports HeWhoMustNotBeNamed's libraries (`rzigzag`, `_matrix`, `arrays` and `strategy`, MPL-2.0); the parts it uses
//! are ported here. Its recursion into higher zigzag levels only scans the first level's pivots again, which finds nothing new,
//! so it is not ported. The rule's stored status never leaves 0, as its `matrix.set` sits inside the branch that tests for 0,
//! so a pattern whose entry has filled and stopped out is entered again at the line's next price, under the same id.
//!
//! Each fill of a pattern's entry gets its own lot name (`id.generation`) with its own exit: the rule's exit orders of one id
//! apply to every trade of that id, and a guard that fired stays spent while lots of its name live, so a second entry filled
//! in the bar of the first one's stop would otherwise go unprotected.
//!
//! shortcut: a run with bound exits (`vike_tactics::RuleRun::bound_exits`), which spends a guard per entry fill, does not free it
//! here; a fix in the order language would drop the per-fill names.
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
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const LENGTH: usize = 8;
const DEPTH: usize = 250;
const KEPT_PATTERNS: usize = 10;
const MIN_RISK_REWARD: f64 = 1.0;

/// A zigzag pivot: `dir` is 1 for a high and -1 for a low, doubled when it beat the pivot two back.
#[derive(Clone, Copy)]
struct Pivot {
    price: f64,
    bar: i64,
    dir: i32,
}

#[derive(Clone, Copy)]
struct Line {
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
}

impl Line {
    fn price(&self, x: f64) -> f64 {
        self.y1 + (self.y2 - self.y1) / (self.x2 - self.x1) * (x - self.x1)
    }
}

/// One registered wedge: the line its entry follows, and its entry, stop and target.
struct Pattern {
    id: u32,
    /// How many of its entries have filled: each fill gets the next lot name, so its exit is its own.
    generation: u32,
    pivots: [i64; 5],
    l2: Line,
    entry: f64,
    stop: f64,
    target: f64,
}

struct S {
    bar_index: i64,
    /// The last 8 bars' channel, with where its extremes lie, and its `[upper, mid, lower]`.
    extremes: Donchian,
    channel: [f64; 3],
    zigzag: Vec<Pivot>,
    /// Newest first, at most `KEPT_PATTERNS`.
    patterns: Vec<Pattern>,
    next_id: u32,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        bar_index: -1,
        extremes: Donchian::new(LENGTH as f64).expect("a valid length").with_extreme_offsets(true),
        channel: [f64::NAN; 3],
        zigzag: Vec::new(),
        patterns: Vec::new(),
        next_id: 1,
    })
}

fn sign(x: f64) -> i32 {
    if x > 0.0 {
        1
    } else if x < 0.0 {
        -1
    } else {
        0
    }
}

impl S {
    /// `addnewpivot`: marks the pivot against the pivot two back, then puts it first.
    fn add_pivot(&mut self, mut p: Pivot) {
        let dir = sign(f64::from(p.dir));
        if let Some(last) = self.zigzag.first() {
            assert_ne!(sign(f64::from(last.dir)), dir, "the rule stops on a direction mismatch");
            if let Some(llast) = self.zigzag.get(1) {
                let d = f64::from(dir);
                p.dir = if d * p.price > d * llast.price { dir * 2 } else { dir };
            }
        }
        self.zigzag.insert(0, p);
        self.zigzag.truncate(DEPTH);
    }

    /// The zigzag's step on this bar: whether it made a new pivot, and a double one.
    fn zigzag_step(&mut self) -> (bool, bool) {
        let bar = self.bar_index;
        // The highest high and lowest low of the last 8 bars and their offsets (`highestbars`, `lowestbars`); a tie goes to
        // the older bar.
        let (mut phigh, mut plow) = (f64::NAN, f64::NAN);
        let (mut phighbar, mut plowbar) = (None, None);
        if let Some([hi, lo]) = self.extremes.extreme_offsets() {
            (phigh, plow) = (self.channel[0], self.channel[2]);
            (phighbar, plowbar) = (Some(-(hi as i64)), Some(-(lo as i64)));
        }
        let mut p_dir = 1;
        let mut distance = 0.0;
        if let Some(last) = self.zigzag.first() {
            p_dir = sign(f64::from(last.dir));
            distance = (bar - last.bar) as f64;
        }
        let mut force_double = false;
        if self.zigzag.len() > 1 {
            let llast = self.zigzag[1].price;
            force_double = if p_dir == 1 && plowbar == Some(0) {
                plow < llast
            } else if p_dir == -1 && phighbar == Some(0) {
                phigh > llast
            } else {
                false
            };
        }
        let overflow = distance >= LENGTH as f64;
        let (mut new_pivot, mut double_pivot) = (false, false);
        if ((p_dir == 1 && phighbar == Some(0)) || (p_dir == -1 && plowbar == Some(0)))
            && !self.zigzag.is_empty()
        {
            let value = if p_dir == 1 { phigh } else { plow };
            let (pivot, pivot_dir) = (self.zigzag[0].price, f64::from(self.zigzag[0].dir));
            if value * pivot_dir > pivot * pivot_dir {
                self.zigzag.remove(0);
                self.add_pivot(Pivot { price: value, bar, dir: p_dir });
                new_pivot = true;
            }
        }
        // `a or b and c` reads `a or (b and c)`.
        if (p_dir == 1 && plowbar == Some(0))
            || (p_dir == -1 && phighbar == Some(0) && (!new_pivot || force_double))
        {
            let value = if p_dir == 1 { plow } else { phigh };
            self.add_pivot(Pivot { price: value, bar, dir: -p_dir });
            double_pivot = new_pivot;
            new_pivot = true;
        }
        if overflow && !new_pivot {
            let (price, offset) = if p_dir == 1 { (plow, plowbar) } else { (phigh, phighbar) };
            let at = bar + offset.expect("a full window");
            self.add_pivot(Pivot { price, bar: at, dir: -p_dir });
            new_pivot = true;
        }
        (new_pivot, double_pivot)
    }

    /// `find_wedge` on the five pivots from row `start`: the wedge it registers and orders, if any.
    fn find_wedge(&mut self, strategy: &mut Tactics<'_>, bar: &Bar, start: usize) {
        let z = &self.zigzag;
        let (p5, p4, p3, p2, p1) =
            (z[start], z[start + 1], z[start + 2], z[start + 3], z[start + 4]);
        let last_dir = f64::from(p5.dir);
        // The trade direction is long: the newest pivot must be a low.
        if last_dir >= 0.0 {
            return;
        }
        let bars = [p1.bar, p2.bar, p3.bar, p4.bar, p5.bar];
        let known = self.patterns.iter().any(|q| {
            let same = bars.iter().zip(q.pivots.iter()).filter(|(a, b)| a == b).count();
            same >= 3
        });
        if known {
            return;
        }
        let (a1, a2, a3, a4, a5) = (p1.price, p2.price, p3.price, p4.price, p5.price);
        let basic = a2 > a1.max(a3).max(a4).max(a5)
            && a5 < a1.min(a2).min(a3).min(a4)
            && a1 > a3
            && a1 < a4;
        if !basic {
            return;
        }
        // The lines through 1 and 5 and through 2 and 4, the second cut to the first's span.
        let l1 = Line { x1: p1.bar as f64, y1: a1, x2: p5.bar as f64, y2: a5 };
        let through = Line { x1: p2.bar as f64, y1: a2, x2: p4.bar as f64, y2: a4 };
        let (start_bar, end_bar) = (p1.bar as f64, p5.bar as f64);
        let (l2_start, l2_end) = (through.price(start_bar), through.price(end_bar));
        let mut l2 = Line { x1: start_bar, y1: l2_start, x2: end_bar, y2: l2_end };
        let width = (end_bar - start_bar).abs();
        let contracting = (a1 - l2_start).abs() > (a5 - l2_end).abs();
        let not_triangle = sign(a1 - a5) == sign(l2_start - l2_end);
        if !(contracting && not_triangle) {
            return;
        }
        let mut closing = None;
        let mut i = end_bar;
        let last = end_bar + 500f64.min(2.0 * width);
        while i <= last {
            let (q1, q2) = (l1.price(i), l2.price(i));
            if last_dir * (q1 - q2) <= 0.0 {
                closing = Some((i, (q1 + q2) / 2.0));
                break;
            }
            i += 1.0;
        }
        let Some((closing_bar, closing_price)) = closing else { return };
        l2.x2 = closing_bar;
        l2.y2 = closing_price;
        let projection = Line { x1: p1.bar as f64, y1: a1, x2: p4.bar as f64, y2: a4 };
        let target = projection.price(closing_bar);
        let stop = closing_price;
        let entry = l2.price((self.bar_index + 1) as f64);
        let id = self.next_id;
        self.next_id += 1;
        self.patterns
            .insert(0, Pattern { id, generation: 0, pivots: bars, l2, entry, stop, target });
        self.patterns.truncate(KEPT_PATTERNS);
        let rr = (entry - target).abs() / (entry - stop).abs();
        let dir = if entry > stop { 1.0 } else { -1.0 };
        if rr >= MIN_RISK_REWARD {
            let generation = &mut self.patterns[0].generation;
            order(strategy, id, generation, entry, stop, target, bar.close * dir > entry * dir);
        }
    }

    /// `evaluate`: every registered pattern against this bar, last first.
    fn evaluate(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let mut i = self.patterns.len();
        while i > 0 {
            i -= 1;
            let p = &self.patterns[i];
            let (entry, stop, target) = (p.entry, p.stop, p.target);
            let rr = (entry - target).abs() / (entry - stop).abs();
            let dir = if entry > stop { 1.0 } else { -1.0 };
            let (reach, _) = if dir > 0.0 { (bar.high, bar.low) } else { (bar.low, bar.high) };
            // The stored status is always 0, so this is the rule's `newStatus`.
            let touched = reach * dir > entry * dir && rr >= MIN_RISK_REWARD;
            if reach * dir > target * dir
                || (touched && bar.close * dir < stop)
                || (!touched && (self.bar_index as f64) > p.l2.x2)
            {
                strategy.cancel(&format!("{}.{}", p.id, p.generation));
                self.patterns.remove(i);
            } else if !touched {
                let new_entry = p.l2.price((self.bar_index + 1) as f64);
                let new_rr = (entry - target).abs() / (entry - stop).abs();
                if new_rr >= MIN_RISK_REWARD {
                    let id = p.id;
                    strategy.cancel(&format!("{id}.{}", p.generation));
                    let limit = bar.close * dir > new_entry * dir;
                    order(
                        strategy,
                        id,
                        &mut self.patterns[i].generation,
                        new_entry,
                        stop,
                        target,
                        limit,
                    );
                    self.patterns[i].entry = new_entry;
                }
            }
        }
    }
}

/// `bracketOrderWithoutLeverage`: a limit or stop entry with its exit, when the levels order as a bracket.
fn order(
    strategy: &mut Tactics<'_>,
    id: u32,
    generation: &mut u32,
    entry: f64,
    stop: f64,
    target: f64,
    limit: bool,
) {
    let sizing = strategy.sizing();
    let dir = if entry > stop && entry < target {
        1
    } else if entry < stop && entry > target {
        -1
    } else {
        0
    };
    if dir == 0 {
        return;
    }
    let side = if dir > 0 { Long } else { Short };
    let mut name = format!("{id}.{generation}");
    if strategy.lots().iter().any(|l| l.name == name) {
        *generation += 1;
        name = format!("{id}.{generation}");
    }
    let size = share_of_worth(&sizing, 10.0, entry);
    if limit {
        strategy.entry(&name, side).size(size).limit(entry);
    } else {
        strategy.entry(&name, side).size(size).stop(entry);
    }
    strategy.exit(&format!("Exit {name}")).from(&name).take(target).stop(stop);
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.05), pyramiding: 10, ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        self.bar_index += 1;
        self.channel = self.extremes.next_hl(bar.high, bar.low);
        let (new_pivot, double_pivot) = self.zigzag_step();
        if new_pivot {
            if double_pivot && self.zigzag.len() >= 6 {
                self.find_wedge(strategy, bar, 1);
            }
            if self.zigzag.len() >= 5 {
                self.find_wedge(strategy, bar, 0);
            }
        }
        self.evaluate(strategy, bar);
    }
}
