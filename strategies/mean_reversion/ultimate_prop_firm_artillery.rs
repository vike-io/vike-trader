// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Ultimate Prop Firm [Artillery]" by ArtilleryTrades, https://www.tradingview.com/script/PkUPYbw7/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2026-08-10, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Pivot reversals inside the New York session, orders filling at the close. Swings are pivots of 3 bars before and 2 after.
//! When flat, long 1 on the bar that confirms a swing low, if its low reaches that swing low plus ATR(14), RSI(14) is 40 or
//! more, the bar closes up and its volume is 0.8 of the 20-bar average or more; short 1 on the mirror at a swing high minus
//! ATR(14) with RSI(14) 60 or less. The position leaves in three parts, 0.4, 0.5 and what is left, at targets 1, 2 and 3.5
//! ATR(14) from the close, all three sharing a stop 1.5 ATR(14) the other way. Everything still held closes on the first bar
//! from 15:55 New York. 0.62 a unit of commission, 1 tick of slippage; starts with 10,000.
//!
//! The session is bars opening 09:30 to 15:55 New York. A UTC day allows five entry signals, and none once the account's
//! worth has fallen 3.5% under its worth at the day's first bar.
//!
//! The header states a 5% margin on both sides: the engine's margin call (`[engine.margin_call]`) refuses an entry the account
//! cannot margin, which still counts against the day's five, and cuts a position it cannot carry. After a cut the three parts
//! sum to more than is held, and the reference fills the parts that share the stop in the order they were placed, so the
//! last finds nothing (`[engine.in_bar] ties_in_placement_order`).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! fill = "close"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Atr, ConfirmedPivot, Rsi, Sma};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{self, Long, Short};
use vike_model::params::Params;
use vike_tactics::helpers::us_minute;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const DAY_MS: i64 = 86_400_000;
const NEW_YORK: i64 = -5;
/// The session's bounds as New York `hhmm`.
const OPENS: i64 = 930;
const CLOSES: i64 = 1555;
const MAX_ENTRIES: u32 = 5;
const MAX_DRAWDOWN: f64 = 3.5;
const STOP_ATRS: f64 = 1.5;
/// Each part's size, `None` for what is left, and its target in ATRs.
const PARTS: [(&str, Option<f64>, f64); 3] =
    [("T1", Some(0.4), 1.0), ("T2", Some(0.5), 2.0), ("T3", None, 3.5)];

struct S {
    atr: Atr,
    pivots: ConfirmedPivot,
    rsi: Rsi,
    volume: Sma,
    day: Option<i64>,
    entries: u32,
    day_worth: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        atr: Atr::new(14.0).expect("a valid length"),
        pivots: ConfirmedPivot::new(3.0, 2.0).expect("valid spans"),
        rsi: Rsi::new(14.0).expect("a valid length"),
        volume: Sma::new(20.0).expect("a valid length"),
        day: None,
        entries: 0,
        day_worth: f64::NAN,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::per_contract(0.62), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let atr = self.atr.next(bar);
        let [high_pivot, low_pivot, last_high, last_low] = self.pivots.next(bar);
        let rsi = self.rsi.next(bar);
        let loud = bar.volume >= self.volume.next_value(bar.volume) * 0.8;
        let minute = us_minute(bar.timestamp, NEW_YORK);
        let clock = minute / 60 * 100 + minute % 60;
        let in_session = (OPENS..=CLOSES).contains(&clock);
        let day = bar.timestamp.div_euclid(DAY_MS);
        let worth = strategy.equity();
        if self.day.is_some_and(|d| d != day) {
            (self.entries, self.day_worth) = (0, worth);
        }
        if self.day.is_none() {
            self.day_worth = worth;
        }
        self.day = Some(day);
        let drawdown = if self.day_worth > 0.0 {
            (self.day_worth - worth) / self.day_worth * 100.0
        } else {
            0.0
        };
        let can = self.entries < MAX_ENTRIES && drawdown < MAX_DRAWDOWN && in_session;
        let flat = strategy.position() == 0.0;
        let long = !low_pivot.is_nan()
            && bar.low <= last_low + atr
            && rsi >= 40.0
            && bar.close > bar.open
            && loud
            && can
            && flat;
        let short = !high_pivot.is_nan()
            && bar.high >= last_high - atr
            && rsi <= 60.0
            && bar.close < bar.open
            && loud
            && can
            && flat;
        if long || short {
            let (name, side, toward): (&str, OrderSide, f64) =
                if long { ("L", Long, 1.0) } else { ("S", Short, -1.0) };
            let stop = bar.close - toward * atr * STOP_ATRS;
            strategy.entry(name, side).size(1.0);
            for (part, size, atrs) in PARTS {
                let take = bar.close + toward * atr * atrs;
                let exit = strategy.exit(&format!("{part}{name}")).from(name);
                if let Some(size) = size {
                    exit.size(size);
                }
                exit.take(take).stop(stop);
            }
            self.entries += 1;
        }
        if clock >= CLOSES && strategy.position() != 0.0 {
            strategy.close_all();
        }
    }
}
