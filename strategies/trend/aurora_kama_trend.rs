// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Aurora KAMA Trend" by blitz_locked, https://www.tradingview.com/script/kbYTJ8V2/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2026-09-12, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Kaufman's adaptive average of the close (efficiency ratio over 10 bars, fast 2, slow 30; it starts at the first close and
//! its ratio reads 0 while the 10-bar sum of changes is still missing). Long 25% of the account's worth when it has risen for
//! 3 bars with the close above SMA(200), short on the mirror, each at the next open, closing and reversing the other, and
//! none until 5 bars after the last entry. Held, every bar: a stop 4% from the average cost and, from the 5th bar in the
//! trade, a trailing stop that waits for 6% past the entry and then follows by 6%, the tick counts cut to whole ticks.
//! 0.05% commission, 1 tick of slippage; version 6: starts with 10,000, margined at 100% (`main.rs`'s rule 4).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! stacked_closes = true
//! ```

use vike_tactics::sizing::share_of_worth;
/// The length of the 30-minute bar the rule was written on.
const BAR_MS: i64 = 1_800_000;
use std::collections::VecDeque;
use vike_tactics::{Rule, RuleDefaults, Tactics, TacticsTrail};

use vike_indicators::indicators::{Kama, Sma};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;

const RISING: usize = 3;
const BETWEEN: usize = 5;
const STOP: f64 = 4.0;
const TRAIL: f64 = 6.0;
const DELAY: i64 = 5;

struct S {
    kama: Kama,
    /// The average's last values, newest first, `RISING + 1` of them.
    seen: VecDeque<f64>,
    trend: Sma,
    bar: usize,
    last_trade: Option<usize>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    // A comparison with a missing value is false, so while the 10-bar sum of changes is missing, or the window is flat, the
    // efficiency ratio reads 0 (the slow speed), and the average starts at the first close.
    let kama = Kama::new(10.0, 2.0, 30.0)
        .expect("valid lengths")
        .with_seed_first(true)
        .with_flat_ratio(0.0)
        .expect("a ratio in 0..=1")
        .with_textbook_steps(true);
    Box::new(S {
        kama,
        seen: VecDeque::with_capacity(RISING + 2),
        trend: Sma::new(200.0).expect("a valid length"),
        bar: 0,
        last_trade: None,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let sizing = strategy.sizing();
        let kama = self.kama.next(bar);
        self.seen.push_front(kama);
        self.seen.truncate(RISING + 1);
        let sma = self.trend.next(bar);
        let steps = || self.seen.iter().zip(self.seen.iter().skip(1));
        let full = self.seen.len() > RISING;
        let rising = full && steps().all(|(a, b)| a > b);
        let falling = full && steps().all(|(a, b)| a < b);
        let cooled = self.last_trade.is_none_or(|t| self.bar - t >= BETWEEN);
        let held = strategy.position();
        if rising && bar.close > sma && cooled && held <= 0.0 {
            strategy.close("Short");
            strategy.entry("Long", Long).size(share_of_worth(&sizing, 25.0, bar.close));
            self.last_trade = Some(self.bar);
        }
        if falling && bar.close < sma && cooled && held >= 0.0 {
            strategy.close("Long");
            strategy.entry("Short", Short).size(share_of_worth(&sizing, 25.0, bar.close));
            self.last_trade = Some(self.bar);
        }
        self.bar += 1;

        let Some(lot) = strategy.lots().first() else { return };
        let in_trade = (bar.timestamp - lot.entry_timestamp) / BAR_MS;
        let cost = strategy.avg_cost();
        let stop = cost * STOP / 100.0;
        let distance = cost * TRAIL / 100.0 / tick;
        let trail = TacticsTrail { after_ticks: distance.floor(), by_ticks: distance.trunc() };
        if held > 0.0 {
            let exit = strategy.exit("Exit Long").from("Long").stop(cost - stop);
            if in_trade >= DELAY {
                exit.trail(trail);
            }
        }
        if held < 0.0 {
            let exit = strategy.exit("Exit Short").from("Short").stop(cost + stop);
            if in_trade >= DELAY {
                exit.trail(trail);
            }
        }
    }
}
