// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "[STRATEGY][RS]Open Session Breakout Trader" by RicardoSantos, https://www.tradingview.com/script/mIvVYU42/
// The original's notice: © RicardoSantos.
// Changed: rewritten by hand in Rust for vike.

//! Two sessions a day, 04:00 to 07:00 and 09:00 to 13:00 New York (the US daylight rule; a bar belongs to one by its open).
//! Each session's first bar sets its open price, and the lines a quarter of the daily range above and below it: the daily
//! range is ATR(100) of the days, as of the day before the last day closed. With no trade open, in a session, long 10,000 when
//! the high crosses up through the upper line (or the session's first bar already reaches over it), short 10,000 when the low
//! crosses down through the lower line (or its first bar reaches under it). Each trade leaves at a target 250 ticks past its
//! fill or a stop 100 ticks against it. No costs; starts with 100,000, kept in the quote's currency. **Does not
//! reproduce its reference** (872 trades, net -187.600006 on CAPITALCOM:EURUSD 30m, 2024-01-01..2026-07-30): checked on
//! Dukascopy's EURUSD, another feed's prices, this file gives 873 / -120.000000 on bars of the mid price, which it is
//! run on (the nearer count), and 877 / -182.200000 on bars of the bid; the owner accepted those numbers on 2026-11-16.
//!
//! The size is 25 * 400 times a multiplier the original keeps from its closed trades: as it writes it, the multiplier for a
//! run of wins stays 1 (it multiplies by 1.0), and the one for a run of losses resets to 1 on every loss and doubles on a win
//! or an even trade; the first is taken after a win, the second after a loss, so a trade is 10,000 unless an even trade comes
//! after a loss. The original stops entering once the account's worth has fallen to 95,000 (its "margin call", on by default).
//! Closing at the session's end, the trailing exit and the plot are off or left out.
//!
//! The day is the symbol's daily input taken as each day closes (`vike_tactics::higher::HigherTimeframeSourceUnseen`), its
//! whole history: a FX day runs from 17:00 New York, which the UTC periods of `vike_tactics::HigherTimeframe` do not build.
//!
//! **Data:** the reference ran on CAPITALCOM:EURUSD, a CFD feed; this file is checked on Dukascopy's EURUSD, a forex feed.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Atr;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::helpers::us_minute;
use vike_tactics::higher::{self, HigherTimeframeSourceUnseen};
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

/// The sessions, in minutes of the New York day: each from its first minute up to, not including, its last.
const SESSIONS: [(i64, i64); 2] = [(4 * 60, 7 * 60), (9 * 60, 13 * 60)];
const RANGE_SHARE: f64 = 0.25;
const MARGIN_CALL: f64 = 95_000.0;
const WINS_MULTIPLIER: f64 = 1.0;
const LOSSES_MULTIPLIER: f64 = 2.0;
const BASE_SIZE: f64 = 25.0 * 400.0;
const TAKE_TICKS: f64 = 250.0;
const STOP_TICKS: f64 = 100.0;

/// The original's `f_martingale_*` (both written alike): 1 on its first bar and after a loss, times `multiplier` after a win
/// or an even trade.
struct Martingale {
    value: f64,
    multiplier: f64,
}

impl Martingale {
    fn step(&mut self, closed: (usize, usize, usize)) -> f64 {
        let (wins, losses, evens) = closed;
        if losses > 0 {
            self.value = 1.0;
        } else if wins > 0 || evens > 0 {
            self.value *= self.multiplier;
        }
        self.value
    }
}

struct S {
    days: HigherTimeframeSourceUnseen,
    atr: Atr,
    /// The daily ATR as of the last day closed, and as of the day before it: the range read.
    atr_now: f64,
    range: f64,
    /// Whether the bar before was in a session, `None` on the first bar (no change to read).
    was_in_session: Option<bool>,
    open_price: f64,
    buy_cross: Cross,
    sell_cross: Cross,
    /// The original's margin call: set once the worth has been at or under `MARGIN_CALL`.
    called: bool,
    /// Wins, losses and evens closed so far.
    counts: (usize, usize, usize),
    wins: Martingale,
    losses: Martingale,
    /// 1 after a win, -1 after a loss.
    mode: i8,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        days: HigherTimeframeSourceUnseen::new(),
        atr: Atr::new(100.0).expect("a valid length"),
        atr_now: f64::NAN,
        range: f64::NAN,
        was_in_session: None,
        open_price: f64::NAN,
        buy_cross: Cross::new(),
        sell_cross: Cross::new(),
        called: false,
        counts: (0, 0, 0),
        wins: Martingale { value: 1.0, multiplier: WINS_MULTIPLIER },
        losses: Martingale { value: 1.0, multiplier: LOSSES_MULTIPLIER },
        mode: 1,
    })
}

/// The exits of what is held, from its fill: a target `TAKE_TICKS` with it, a stop `STOP_TICKS` against it.
fn exits(strategy: &mut Tactics<'_>) {
    let Some(tick) = strategy.tick() else { return };
    let (held, cost) = (strategy.position(), strategy.avg_cost());
    let (take, stop) = (TAKE_TICKS * tick, STOP_TICKS * tick);
    if held > 0.0 {
        strategy.exit("B").from("B").take(cost + take).stop(cost - stop);
    } else if held < 0.0 {
        strategy.exit("S").from("S").take(cost - take).stop(cost + stop);
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn inputs(&self) -> Vec<String> {
        vec![higher::DAY.to_string()]
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        for day in self.days.next(strategy.input(higher::DAY)) {
            self.range = self.atr_now;
            self.atr_now = self.atr.next(day);
        }
        let minute = us_minute(bar.timestamp, -5);
        let in_session = SESSIONS.iter().any(|&(from, to)| (from..to).contains(&minute));
        let was_in_session = self.was_in_session.replace(in_session);
        let session_starts = in_session && was_in_session == Some(false);
        let open_before = self.open_price;
        if session_starts {
            self.open_price = bar.open;
        }
        let buy_line = self.open_price + self.range * RANGE_SHARE;
        let sell_line = self.open_price - self.range * RANGE_SHARE;
        // `ta.change(open_price) != 0`: false while the open before was missing.
        let new_open = !open_before.is_nan() && self.open_price != open_before;
        let buy = matches!(self.buy_cross.step(bar.high, buy_line), Signal::Long)
            || (new_open && bar.high > buy_line);
        let sell = matches!(self.sell_cross.step(bar.low, sell_line), Signal::Short)
            || (new_open && bar.low < sell_line);

        if strategy.equity() <= MARGIN_CALL {
            self.called = true;
        }
        let flat = strategy.lots().is_empty();

        let closed = strategy.closed_trades().unwrap_or(&[]);
        let counts = closed.iter().fold((0, 0, 0), |(w, l, e), row| match row.net {
            n if n > 0.0 => (w + 1, l, e),
            n if n < 0.0 => (w, l + 1, e),
            _ => (w, l, e + 1),
        });
        let change = (counts.0 - self.counts.0, counts.1 - self.counts.1, counts.2 - self.counts.2);
        self.counts = counts;
        let wins = self.wins.step(change);
        let losses = self.losses.step(change);
        if change.0 > 0 {
            self.mode = 1;
        } else if change.1 > 0 {
            self.mode = -1;
        }
        let size = BASE_SIZE * if self.mode > 0 { wins } else { losses };

        if in_session && !self.called && flat {
            if buy {
                strategy.entry("B", Long).size(size);
            }
            if sell {
                strategy.entry("S", Short).size(size);
            }
        }
        exits(strategy);
    }

    fn after_fill(&mut self, strategy: &mut Tactics<'_>, _bar: &Bar) {
        exits(strategy);
    }
}
