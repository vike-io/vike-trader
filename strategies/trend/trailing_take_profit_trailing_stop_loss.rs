// SPDX-License-Identifier: MIT
// Copyright © 2022-present Iason Nikolas | jason5480. Licensed under the MIT license; its text, with this notice, is LICENSE-MIT in the strategies folder.
// Derived from "Trailing Take Profit Trailing Stop Loss" by jason5480, https://www.tradingview.com/script/V7IfK3mr/
// Changed: rewritten by hand in Rust for vike.

//! SMA(21) and SMA(49) of the close. Their cross up opens a long of all of the account's worth at the next open and closes a
//! short; their cross down opens a short and closes a long; a signal on the side already held opens nothing. While a side is
//! held it carries two exits, re-stated every bar:
//! - a take profit for half of the entry (floored to the step): a trailing exit that arms at 10% past the signal bar's close
//!   (rounded to the tick toward the position) and then follows the best price by 1% of that level (cut to whole ticks), with
//!   the stop below as its other leg;
//! - a stop for the rest, 7.5% against the signal bar's close on that bar and then against the entry price, never loosened.
//!
//! Once the take profit has worked (the open trade shrank, or the bar the entry filled on reached the arming level) the stop
//! follows the bar's high (a long's; a short's low) by 7.5%, never loosened, and the take profit loses its trailing leg: still
//! there, it is a stop for its half. No costs; starts with 100,000. **Does not reproduce its reference** (1308 trades, net
//! -22222.197266 on BTCUSDT 30m, 2025-01-01..2026-07-30): this file gives 1308 / -21064.002785, the net 5.2% off with no
//! cause found (no stop or trail fills on this window, only reversals and margin cuts, so the gap is in the margin call or
//! in the size of the reversals of all of the account's worth); the owner accepted those numbers on 2026-11-15.
//!
//! The rule reads its date window from the imported library `chrono_utils` 7 (by jason5480, MPL-2.0); with no date set the
//! window includes every bar, so nothing of it is ported. The size of "the last open trade" reads 0 while flat.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Sma;
use vike_marketdata::Bar;
use vike_model::OrderSide::{self, Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::{floor_share, share_of_worth};
use vike_tactics::{Rule, RuleDefaults, Tactics, TacticsTrail};

const STOP: f64 = 7.5 / 100.0;
const TAKE_PROFIT: f64 = 10.0 / 100.0;
const DISTANCE: f64 = 1.0 / 100.0;
const SHARE: f64 = 50.0 / 100.0;
/// The level a short's stop starts from when it has none yet.
const NO_SHORT_STOP: f64 = 999_999.9;

/// One side's state.
struct Side {
    entry: &'static str,
    take_exit: &'static str,
    stop_exit: &'static str,
    /// 1 for the long side, -1 for the short.
    way: f64,
    stop: f64,
    take: f64,
    /// Whether the take profit has worked (its target reached or part of the trade closed).
    taken: bool,
    /// The entry still to fill, with the stop and the take profit level of its signal bar.
    pending: Option<(f64, f64)>,
    trail: Option<TacticsTrail>,
}

impl Side {
    fn new(
        entry: &'static str,
        take_exit: &'static str,
        stop_exit: &'static str,
        way: f64,
    ) -> Side {
        Side {
            entry,
            take_exit,
            stop_exit,
            way,
            stop: f64::NAN,
            take: f64::NAN,
            taken: false,
            pending: None,
            trail: None,
        }
    }

    /// The stop `STOP` against `price` on this side.
    fn stop_from(&self, price: f64) -> f64 {
        price * (1.0 - self.way * STOP)
    }

    /// Never loosened: the higher of a long's two stops, the lower of a short's (a missing one is 0, a short's 999,999.9).
    fn tighter(&self, stop: f64, before: f64) -> f64 {
        if self.way > 0.0 {
            stop.max(if before.is_nan() { 0.0 } else { before })
        } else {
            stop.min(if before.is_nan() { NO_SHORT_STOP } else { before })
        }
    }

    /// The take profit's trail from an entry at `cost`, its arming level `take` rounded to the tick toward the position.
    fn trail_of(&self, take: f64, cost: f64, tick: f64) -> TacticsTrail {
        let arm =
            if self.way > 0.0 { (take / tick + 1e-4).floor() } else { (take / tick - 1e-4).ceil() };
        let after = self.way * (arm - (cost / tick).round());
        TacticsTrail { after_ticks: after, by_ticks: (take * DISTANCE / tick).trunc() }
    }

    /// The two exits, as they stand: the take profit (with its trail while it has a level) and the stop for the rest.
    fn exits(&self, strategy: &mut Tactics<'_>, stop: f64, half: f64) {
        let exit = strategy.exit(self.take_exit);
        exit.from(self.entry).size(half).stop(stop);
        if let (false, Some(trail)) = (self.take.is_nan(), self.trail) {
            exit.trail(trail);
        }
        strategy.exit(self.stop_exit).from(self.entry).stop(stop);
    }
}

struct S {
    fast: Sma,
    slow: Sma,
    cross: Cross,
    long: Side,
    short: Side,
    /// The signed size of the last open trade on the bar before.
    last_before: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: Sma::new(21.0).expect("a valid length"),
        slow: Sma::new(49.0).expect("a valid length"),
        cross: Cross::new(),
        long: Side::new("Long Entry", "Long Take Profit / Stop Loss", "Long Stop Loss", 1.0),
        short: Side::new("Short Entry", "Short Take Profit / Stop Loss", "Short Stop Loss", -1.0),
        last_before: 0.0,
    })
}

/// The signed size of the last open trade, 0 while flat, and its entry price.
fn last_trade(strategy: &Tactics<'_>) -> (f64, f64) {
    strategy.lots().last().map_or((0.0, f64::NAN), |lot| {
        let way = if lot.side == Long { 1.0 } else { -1.0 };
        (way * lot.qty, lot.price)
    })
}

/// Half of what the entry `name` still holds, floored to the step.
fn half_of(strategy: &Tactics<'_>, name: &str) -> f64 {
    let step = strategy.sizing().qty_step();
    strategy
        .bindings()
        .iter()
        .find(|b| b.name == name)
        .map_or(0.0, |b| floor_share(SHARE * b.left, b.left, step))
}

/// What one side does on this bar, its levels worked out (`Side::levels`).
struct Plan {
    valid: bool,
    active: bool,
    holds: bool,
}

impl Side {
    /// This side's levels for the bar: `opens` is its signal, `closes` the other side's; `last` the signed size of the last
    /// open trade now and `last_before` on the bar before, `entry_price` its entry price.
    fn levels(
        &mut self,
        bar: &Bar,
        opens: bool,
        closes: bool,
        (last, last_before, entry_price): (f64, f64, f64),
    ) -> Plan {
        let side = self;
        let holds = side.way * last > 0.0;
        let valid = opens && !holds;
        let active = valid || holds && !closes;
        side.stop = if !active {
            f64::NAN
        } else if valid {
            side.stop_from(bar.close)
        } else {
            let base = if side.taken {
                if side.way > 0.0 { bar.high } else { bar.low }
            } else {
                entry_price
            };
            side.tighter(side.stop_from(base), side.stop)
        };
        side.take = if active && !side.taken {
            let level = bar.close * (1.0 + side.way * TAKE_PROFIT);
            if valid || side.take.is_nan() { level } else { side.take }
        } else {
            f64::NAN
        };
        let reached = if side.way > 0.0 { bar.high >= side.take } else { bar.low <= side.take };
        side.taken = holds
            && (side.taken
                || side.way * last < side.way * last_before
                || last_before == 0.0 && reached);
        Plan { valid, active, holds }
    }

    /// This side's entry on its signal (its exits wait for the fill, `Side::filled`), else its exits while it is held.
    fn orders(&mut self, strategy: &mut Tactics<'_>, bar: &Bar, plan: &Plan) {
        if plan.valid {
            let way: OrderSide = if self.way > 0.0 { Long } else { Short };
            let size = share_of_worth(&strategy.sizing(), 100.0, bar.close);
            strategy.entry(self.entry, way).size(size);
            self.pending = Some((self.stop, self.take));
        } else if plan.active && plan.holds {
            let half = half_of(strategy, self.entry);
            self.exits(strategy, self.stop, half);
        }
    }

    /// Right after an entry of this side filled: its exits, with its signal bar's levels and the trail from the fill.
    fn filled(&mut self, strategy: &mut Tactics<'_>, tick: f64) {
        let Some((stop, take)) = self.pending else { return };
        if self.way * strategy.position() <= 0.0 {
            return;
        }
        self.pending = None;
        self.trail = Some(self.trail_of(take, strategy.avg_cost(), tick));
        let half = half_of(strategy, self.entry);
        self.exits(strategy, stop, half);
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        if strategy.tick().is_none() {
            return; // its take-profit trail counts in ticks
        }
        let signal = self.cross.step(self.fast.next(bar), self.slow.next(bar));
        let (up, down) = (matches!(signal, Signal::Long), matches!(signal, Signal::Short));
        let (last, entry_price) = last_trade(strategy);
        let before = std::mem::replace(&mut self.last_before, last);
        let reads = (last, before, entry_price);
        let long = self.long.levels(bar, up, down, reads);
        let short = self.short.levels(bar, down, up, reads);
        // The original's order: both closes, then the long side's entry and exits, then the short side's.
        if down {
            strategy.close(self.long.entry);
        }
        if up {
            strategy.close(self.short.entry);
        }
        self.long.orders(strategy, bar, &long);
        self.short.orders(strategy, bar, &short);
    }

    fn after_fill(&mut self, strategy: &mut Tactics<'_>, _bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        self.long.filled(strategy, tick);
        self.short.filled(strategy, tick);
    }
}
