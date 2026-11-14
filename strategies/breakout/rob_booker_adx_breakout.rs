// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Rob Booker - ADX Breakout" by Powerscooter, https://www.tradingview.com/script/qoaam9SI/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2022-08-10, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! A breakout of a quiet market, 100,000 cash a trade. The box is the highest high and the lowest low of the 20 bars before
//! this one, held as it stood at the entry for as long as a position is open. Flat, with ADX(14) under 18, long when the close
//! crosses the box's top either way, short when it crosses its bottom either way. While held, an exit targets one box width
//! past the average price and stops half a width the other way. No costs; starts with 100,000.
//!
//! The ADX is the rule's own: the DI lines are Wilder averages (SMA-seeded) of the moves and of the true range, each held at
//! its last value while it has none, and the ADX a Wilder average of `|+DI - -DI| / (+DI + -DI)`, a zero sum read as 1. A
//! move against the missing bar before the first counts as 0 where the true range is missing (`Adx::with_first_bar_moves`,
//! `Adx::with_held_lines`).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Adx, Donchian};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::rolling::CrossEither;
use vike_tactics::sizing::cash_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const LONG_NAME: &str = "open_long";
const SHORT_NAME: &str = "open_short";
const CASH: f64 = 100_000.0;
const ADX_LOW: f64 = 18.0;
const TARGET_WIDTHS: f64 = 1.0;
const STOP_WIDTHS: f64 = 0.5;

struct S {
    dmi: Adx,
    window: Donchian,
    /// The last bar's highest high and lowest low of 20.
    last_window: (f64, f64),
    /// The box, carried while a position is open.
    top: f64,
    bottom: f64,
    top_cross: CrossEither,
    bottom_cross: CrossEither,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        dmi: Adx::new(14.0)
            .expect("a valid length")
            .with_first_bar_moves(true)
            .with_held_lines(true),
        window: Donchian::new(20.0).expect("a valid length"),
        last_window: (f64::NAN, f64::NAN),
        top: 0.0,
        bottom: 0.0,
        top_cross: CrossEither::default(),
        bottom_cross: CrossEither::default(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let [adx, _, _] = self.dmi.next(bar);
        let [highest, _, lowest] = self.window.next(bar);
        let (top, bottom) = std::mem::replace(&mut self.last_window, (highest, lowest));
        let held = strategy.position();
        if held == 0.0 {
            (self.top, self.bottom) = (top, bottom);
        }
        let width = self.top - self.bottom;
        let quiet = adx < ADX_LOW;
        // Each cross steps on every bar, never skipped by an `and` that stopped at an earlier false.
        let up = self.top_cross.step(bar.close, self.top);
        let down = self.bottom_cross.step(bar.close, self.bottom);
        let buy = held == 0.0 && up && quiet;
        let sell = held == 0.0 && down && quiet;

        if held > 0.0 {
            let cost = strategy.avg_cost();
            strategy
                .exit("close_long")
                .from(LONG_NAME)
                .take(cost + TARGET_WIDTHS * width)
                .stop(cost - STOP_WIDTHS * width);
        }
        if buy {
            strategy.entry(LONG_NAME, Long).size(cash_worth(&sizing, CASH, bar.close));
        }
        if held < 0.0 {
            let cost = strategy.avg_cost();
            strategy
                .exit("close_short")
                .from(SHORT_NAME)
                .take(cost - TARGET_WIDTHS * width)
                .stop(cost + STOP_WIDTHS * width);
        }
        if sell {
            strategy.entry(SHORT_NAME, Short).size(cash_worth(&sizing, CASH, bar.close));
        }
    }
}
