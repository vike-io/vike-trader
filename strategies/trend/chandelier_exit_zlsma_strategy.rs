// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Chandelier Exit ZLSMA Strategy" by simwai, https://www.tradingview.com/script/QJRHUWcF/
// The original's notice: © simwai
// Changed: rewritten by hand in Rust for vike.

//! A chandelier exit and a zero-lag least-squares line, both on the bar's mean price `(open + high + low + close) / 4`.
//! The exit's long stop is that price less twice the bar's true range, ratcheted up while the price stays over the previous
//! stop; its short stop is the mirror. Its direction turns up when the price closes over the previous short stop and down
//! when it closes under the previous long stop. The line is `2 * LR - LR(LR)`, `LR` the 50-bar linear regression.
//!
//! Long 10% of the account's worth on the bar the direction turns up and the price crosses up through the line; short on
//! the mirror. A long closes when the price crosses down through the line, a short when it crosses up. The rule tracks the
//! trade it believes open: a new entry on that side is dropped, and an entry the other way on the bar that closes it waits
//! for the next bar. Every order fills at this bar's close. 0.075% commission; starts with 1,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! fill = "close"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Atr, Linearreg};
use vike_indicators::strategy_kit::{max, min};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const SHARE: f64 = 10.0;
const MULTIPLIER: f64 = 2.0;
const LINE: f64 = 50.0;

#[derive(Clone, Copy, PartialEq)]
enum Open {
    None,
    Long,
    Short,
}

struct S {
    atr: Atr,
    long_stop: f64,
    short_stop: f64,
    /// `1` up, `-1` down; the previous bar's, NaN on the first.
    dir: i8,
    dir_previous: Option<i8>,
    lsma: Linearreg,
    lsma2: Linearreg,
    cross: Cross,
    open: Open,
    /// An entry left over from the bar that closed the other side.
    cached: Open,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        atr: Atr::new(1.0).expect("a valid length"),
        long_stop: f64::NAN,
        short_stop: f64::NAN,
        dir: 1,
        dir_previous: None,
        lsma: Linearreg::new(LINE).expect("a valid length"),
        lsma2: Linearreg::new(LINE).expect("a valid length"),
        cross: Cross::new(),
        open: Open::None,
        cached: Open::None,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.075), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let price = 1.0 / 4.0 * (bar.open + bar.high + bar.low + bar.close);
        let atr = MULTIPLIER * self.atr.next(bar);

        let long_stop = price - atr;
        let long_previous = if self.long_stop.is_nan() { long_stop } else { self.long_stop };
        self.long_stop =
            if price > long_previous { max(long_stop, long_previous) } else { long_stop };
        let short_stop = price + atr;
        let short_previous = if self.short_stop.is_nan() { short_stop } else { self.short_stop };
        self.short_stop =
            if price < short_previous { min(short_stop, short_previous) } else { short_stop };
        let dir_previous = self.dir_previous;
        if price > short_previous {
            self.dir = 1;
        } else if price < long_previous {
            self.dir = -1;
        }
        self.dir_previous = Some(self.dir);
        let buy = self.dir == 1 && dir_previous == Some(-1);
        let sell = self.dir == -1 && dir_previous == Some(1);

        let lsma = self.lsma.next_value(price);
        let lsma2 = self.lsma2.next_value(lsma);
        let line = lsma + (lsma - lsma2);
        let cross = self.cross.step(price, line);
        let (up, down) = (matches!(cross, Signal::Long), matches!(cross, Signal::Short));

        let mut enter_long = buy && up;
        let mut exit_long = down;
        let mut enter_short = sell && down;
        let mut exit_short = up;
        match std::mem::replace(&mut self.cached, Open::None) {
            Open::Long => enter_long = true,
            Open::Short => enter_short = true,
            Open::None => {}
        }
        match self.open {
            Open::None => {
                if exit_short && !enter_long {
                    exit_short = false;
                }
                if exit_long && !enter_short {
                    exit_long = false;
                }
                if enter_long && exit_short {
                    self.open = Open::Long;
                    exit_short = false;
                } else if enter_short && exit_long {
                    self.open = Open::Short;
                    exit_long = false;
                } else if enter_long {
                    self.open = Open::Long;
                } else if enter_short {
                    self.open = Open::Short;
                }
            }
            Open::Long => {
                exit_short = false;
                enter_long = false;
                if enter_short && exit_long {
                    enter_short = false;
                    self.cached = Open::Short;
                }
                if exit_long {
                    self.open = Open::None;
                }
            }
            Open::Short => {
                exit_long = false;
                enter_short = false;
                if enter_long && exit_short {
                    enter_long = false;
                    self.cached = Open::Long;
                }
                if exit_short {
                    self.open = Open::None;
                }
            }
        }

        if exit_long && strategy.position() > 0.0 {
            strategy.close("long");
        }
        if exit_short && strategy.position() < 0.0 {
            strategy.close("short");
        }
        if enter_long {
            strategy.entry("long", Long).size(share_of_worth(&sizing, SHARE, bar.close));
        }
        if enter_short {
            strategy.entry("short", Short).size(share_of_worth(&sizing, SHARE, bar.close));
        }
    }
}
