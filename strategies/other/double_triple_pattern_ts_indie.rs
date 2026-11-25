// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Double&Triple Pattern[TS_Indie]" by Truth_Strategy_Indie, https://www.tradingview.com/script/ykzrcWQf/
// The original's notice: © Truth_Strategy_Indie
// Changed: rewritten by hand in Rust for vike.

//! Double and triple tops and bottoms read off swing structure, all of the account's worth on a limit, with a stop and a
//! target 2 times as far the other way. No costs; starts with 100,000. The script is version 6 and states no size, which its
//! language reads as 100% of equity.
//!
//! The structure starts up when the window's first bar rises. While down it tracks the last bar that made a new low, and a
//! close above that bar's high turns it up and records a swing low (that bar's low, or this one's if lower); while up the
//! mirror records a swing high. Each list keeps the newest 11.
//!
//! When the structure turns up with five swing highs on record, and the newest low `L0` lies 55-75% of the way down from the
//! last high `H0` to the low before `L1`, the low before that `L2` 50-75% down, and the high between `H1` 65-110% up from
//! `L1`, the setup is confirmed: any pending entry is cancelled, a pending sell setup dropped, and when flat a buy setup is
//! armed. It fires on the first close at or above `H0` while flat, placing a limit buy at `H0` with a stop at the higher of
//! `L0` and `L1`; a close at or below the lower of the two first drops it. A sell is the mirror on the structure turning down.
//!
//! The rule's exit waits for its entry to fill (`vike_tactics::RuleRun::bound_exits`), so it is issued once, with the entry.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;

const REWARD: f64 = 2.0;
const KEPT: usize = 11;
const LONG_NAME: &str = "long";
const SHORT_NAME: &str = "short";

/// Adds the newest value at the front, dropping the 12th.
fn record(list: &mut VecDeque<f64>, x: f64) {
    list.push_front(x);
    list.truncate(KEPT);
}

/// `x` lies between the two bounds, both included.
fn within(x: f64, a: f64, b: f64) -> bool {
    x <= a && x >= b
}

/// An armed setup: the close that fires it, the close that drops it, and the stop.
#[derive(Clone, Copy)]
struct Setup {
    entry: f64,
    abandon: f64,
    stop: f64,
}

struct S {
    up: bool,
    top: f64,
    bottom: f64,
    highs: VecDeque<f64>,
    lows: VecDeque<f64>,
    buy: Option<Setup>,
    sell: Option<Setup>,
    pending_buy: bool,
    pending_sell: bool,
    closed: usize,
    /// The last entry's exit: its guard name, the entry's name, target and stop.
    exit: Option<(&'static str, &'static str, f64, f64)>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        up: false,
        top: f64::NAN,
        bottom: f64::NAN,
        highs: VecDeque::with_capacity(KEPT + 1),
        lows: VecDeque::with_capacity(KEPT + 1),
        buy: None,
        sell: None,
        pending_buy: false,
        pending_sell: false,
        closed: 0,
        exit: None,
    })
}

impl S {
    /// The last entry's exit, on the entry still to come; it rests from that entry's fill and dies with its lot.
    fn arm_exit(&self, strategy: &mut Tactics<'_>) {
        if let Some((name, lots, take, cut)) = self.exit {
            strategy.exit(name).from(lots).take(take).stop(cut);
        }
    }

    /// One bar of the structure: `(turned up, turned down)`.
    fn structure(&mut self, bar: &Bar) -> (bool, bool) {
        if self.top.is_nan() {
            self.up = bar.close > bar.open;
            (self.top, self.bottom) = (bar.high, bar.low);
        }
        let (mut turned_up, mut turned_down) = (false, false);
        if !self.up {
            if bar.close > self.top {
                record(&mut self.lows, bar.low.min(self.bottom));
                self.up = true;
                turned_up = true;
                (self.top, self.bottom) = (bar.high, bar.low);
            }
            if !turned_up && bar.low <= self.bottom {
                (self.top, self.bottom) = (bar.high, bar.low);
            }
        }
        if self.up {
            if bar.close < self.bottom {
                record(&mut self.highs, bar.high.max(self.top));
                self.up = false;
                turned_down = true;
                (self.top, self.bottom) = (bar.high, bar.low);
            }
            if !turned_down && bar.high >= self.top {
                (self.top, self.bottom) = (bar.high, bar.low);
            }
        }
        (turned_up, turned_down)
    }

    /// Drops the pending entries, as a confirmed setup of either side does.
    fn cancel(&mut self, strategy: &mut Tactics<'_>) {
        if self.pending_sell {
            strategy.cancel(SHORT_NAME);
            self.pending_sell = false;
        }
        if self.pending_buy {
            strategy.cancel(LONG_NAME);
            self.pending_buy = false;
        }
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let (turned_up, turned_down) = self.structure(bar);
        let held = strategy.position();
        let done = strategy.closed_trades().unwrap_or(&[]).len();
        let closed = std::mem::replace(&mut self.closed, done);
        let closed_now = done > closed;
        let mut buy_signal = None;
        let mut sell_signal = None;

        if held > 0.0 || (closed_now && self.pending_buy) {
            self.pending_buy = false;
        }
        // The rule reads five of the other list too, which always has them by then.
        if turned_up && self.highs.len() > 4 && self.lows.len() > 4 {
            let (h0, h1) = (self.highs[0], self.highs[1]);
            let (l0, l1, l2) = (self.lows[0], self.lows[1], self.lows[2]);
            let span = h0 - l1;
            let first = within(l0, h0 - 0.55 * span, h0 - 0.75 * span);
            let second = within(l2, h0 - 0.50 * span, h0 - 0.75 * span);
            let third = within(h1, l1 + 1.10 * span, l1 + 0.65 * span);
            if first && second && third {
                self.sell = None;
                self.cancel(strategy);
                if held == 0.0 {
                    let (low, high) = if l0 < l1 { (l0, l1) } else { (l1, l0) };
                    self.buy = Some(Setup { entry: h0, abandon: low, stop: high });
                }
            }
        }
        if let Some(setup) = self.buy.filter(|_| held == 0.0) {
            if bar.close >= setup.entry {
                self.buy = None;
                buy_signal = Some(setup);
            } else if bar.close <= setup.abandon {
                self.buy = None;
            }
        }

        if held < 0.0 || (closed_now && self.pending_sell) {
            self.pending_sell = false;
        }
        if turned_down && self.lows.len() > 4 && self.highs.len() > 4 {
            let (h0, h1, h2) = (self.highs[0], self.highs[1], self.highs[2]);
            let (l0, l1) = (self.lows[0], self.lows[1]);
            let span = h1 - l0;
            let first = within(h0, l0 + 0.75 * span, l0 + 0.55 * span);
            let second = within(h2, l0 + 0.75 * span, l0 + 0.50 * span);
            let third = within(l1, h1 - 0.65 * span, h1 - 1.10 * span);
            if first && second && third {
                self.buy = None;
                self.cancel(strategy);
                if held == 0.0 {
                    let (low, high) = if h0 > h1 { (h1, h0) } else { (h0, h1) };
                    self.sell = Some(Setup { entry: l0, abandon: high, stop: low });
                }
            }
        }
        if let Some(setup) = self.sell.filter(|_| held == 0.0) {
            if bar.close <= setup.entry {
                self.sell = None;
                sell_signal = Some(setup);
            } else if bar.close >= setup.abandon {
                self.sell = None;
            }
        }

        if let Some(setup) = buy_signal {
            let take = setup.entry + (setup.entry - setup.stop) * REWARD;
            strategy
                .entry(LONG_NAME, Long)
                .size(share_of_worth(&sizing, 100.0, setup.entry))
                .limit(setup.entry);
            self.exit = Some(("long exit", LONG_NAME, take, setup.stop));
            self.arm_exit(strategy);
            self.pending_buy = true;
        }
        if let Some(setup) = sell_signal {
            let take = setup.entry - (setup.stop - setup.entry) * REWARD;
            strategy
                .entry(SHORT_NAME, Short)
                .size(share_of_worth(&sizing, 100.0, setup.entry))
                .limit(setup.entry);
            self.exit = Some(("short exit", SHORT_NAME, take, setup.stop));
            self.arm_exit(strategy);
            self.pending_sell = true;
        }
    }
}
