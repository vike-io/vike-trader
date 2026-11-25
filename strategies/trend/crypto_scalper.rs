// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Crypto Scalper" by exlux, https://www.tradingview.com/script/84YDnJ9V/
// The original's notice: © exlux99
// Changed: rewritten by hand in Rust for vike.

//! A parabolic SAR (0.02, 0.02, 0.2, the textbook law started on the second bar), EMA(60) of the close and the MACD (12, 26,
//! 9) histogram. While the SAR is under the price, the histogram above 0 and the close under the EMA: a short stop at the
//! SAR's next level, all of the account's worth, with a target 5.5% and a stop 3% of this close from its fill. The mirror
//! for a long stop, with a target 24.5% and a stop 100% of this close away. Each entry rests until it fills or the next
//! signal moves it; a signal while that side is held moves its exit's distances. 0.03% commission; starts with 100.
//!
//! The exit's levels exist only once its entry fills, so they are set in `after_fill` and rest from the next point of the walk.
//!
//! The entry rests as a free stop, its size fixed when placed as the original fixes it, the part that closes the position held
//! then included. A short's stop and a long entry can both lie above the price, and fill in the order the price meets them.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Ema, Macd, Psar};
use vike_indicators::strategy_kit::parabolic_sar;
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const START: f64 = 0.02;
const STEP: f64 = 0.02;
const MAX: f64 = 0.2;
const COMMISSION: f64 = 0.03;

/// One side: its entry's name, its exit's name, the exit's `(target, stop)` distances as last signalled, the entry resting
/// as `(level, size)`, and the stop its exit rests at while held.
struct Side {
    side: vike_model::OrderSide,
    entry: &'static str,
    exit: &'static str,
    profit: f64,
    loss: f64,
    distances: Option<(f64, f64)>,
    pending: Option<(f64, f64)>,
    cut: f64,
}

impl Side {
    fn new(
        side: vike_model::OrderSide,
        entry: &'static str,
        exit: &'static str,
        profit: f64,
        loss: f64,
    ) -> Self {
        Side { side, entry, exit, profit, loss, distances: None, pending: None, cut: f64::NAN }
    }

    fn guard(&mut self, strategy: &mut Tactics<'_>) {
        let Some((take, cut)) = self.distances else { return };
        let fill = strategy.avg_cost();
        let toward = -f64::from(self.side);
        self.cut = fill + toward * cut;
        strategy.exit(self.exit).from(self.entry).take(fill - toward * take).stop(self.cut);
    }

    fn rest_entry(&self, strategy: &mut Tactics<'_>) {
        if let Some((level, size)) = self.pending {
            strategy.order(self.entry, self.side).size(size).stop(level);
        }
    }
}

struct S {
    sar: Psar,
    trend: Ema,
    macd: Macd,
    long: Side,
    short: Side,
    held: f64,
    /// The rows closed before this bar.
    rows: usize,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        sar: parabolic_sar(START, STEP, MAX),
        trend: Ema::new(60.0).expect("a valid length"),
        macd: Macd::new(12.0, 26.0, 9.0).expect("valid lengths"),
        long: Side::new(Long, "long", "long exit", 0.245, 1.0),
        short: Side::new(Short, "short", "short exit", 0.055, 0.03),
        held: 0.0,
        rows: 0,
    })
}

impl S {
    /// An entry stop at `level` and its exit's distances; the size is spent at the level, or at this close if already past,
    /// and an entry against the position held also closes it. With that side held the entry is refused.
    fn place(&mut self, strategy: &mut Tactics<'_>, bar: &Bar, long: bool, level: f64) {
        let Some(tick) = strategy.tick() else { return };
        let sizing = strategy.sizing();
        let held = strategy.position();
        let side = if long { &mut self.long } else { &mut self.short };
        let ticks = |share: f64| bar.close * share / tick;
        side.distances = Some((tick * ticks(side.profit), tick * ticks(side.loss)));
        if held != 0.0 && (held > 0.0) == long {
            side.guard(strategy);
            return;
        }
        let stop = strategy.snap_stop(side.side, level);
        let price = if long { stop.max(bar.close) } else { stop.min(bar.close) };
        let size = share_of_worth(&sizing, 100.0, price) + held.abs();
        side.pending = (size > 0.0).then_some((level, size));
        side.rest_entry(strategy);
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(COMMISSION), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        self.rows = strategy.closed_trades().unwrap_or(&[]).len();
        self.sar.next(bar);
        let next = self.sar.stop_ahead();
        // The trend is unset on the first bar, and the histogram long after it.
        let up = self.sar.uptrend() == Some(true);
        let trend = self.trend.next(bar);
        let [_, _, hist] = self.macd.next(bar);
        if up && hist > 0.0 && bar.close < trend {
            self.place(strategy, bar, false, next);
        }
        if !up && hist < 0.0 && bar.close > trend {
            self.place(strategy, bar, true, next);
        }
        self.held = strategy.position();
    }

    // The exits' levels are measured from the entry's fill, so they are issued here, after it; where the run fills a bar's
    // orders in the order the price meets them, they work from the fill's own level on.
    fn after_fill(&mut self, strategy: &mut Tactics<'_>, _bar: &Bar) {
        let held = strategy.position();
        let done = strategy.closed_trades().unwrap_or(&[]);
        let rows = &done[self.rows.min(done.len())..];
        // An entry's own fill: the position is now its side, or it closed some of the other at the entry's level.
        let filled = |side: &Side| {
            side.pending.is_some_and(|(level, _)| {
                (held != 0.0 && (held > 0.0) == (side.side == Long))
                    || rows.iter().any(|r| r.exit_price == strategy.snap_stop(side.side, level))
            })
        };
        let opened = held != 0.0 && (self.held == 0.0 || (held > 0.0) != (self.held > 0.0));
        if held > self.held && filled(&self.long) {
            self.long.pending = None;
        }
        if held < self.held && filled(&self.short) {
            self.short.pending = None;
        }
        if opened && held > 0.0 {
            self.long.guard(strategy);
        }
        if opened && held < 0.0 {
            self.short.guard(strategy);
        }
        if held == 0.0 {
            (self.long.cut, self.short.cut) = (f64::NAN, f64::NAN);
        }
        self.held = held;
    }
}
