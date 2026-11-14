// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Advanced Position Management [Mr_Rakun]" by Mr_Rakun, https://www.tradingview.com/script/IsMKlnoJ/
// The original's notice: © Mr_Rakun
// Changed: rewritten by hand in Rust for vike.

//! Long when EMA(10) crosses up through EMA(20) while flat or short, short on the cross down while flat or long, the opposite
//! entry reversing. Each side enters one fixed size: three times 30% of 50,000 worth at that side's first entry close, each
//! third rounded to 0.001. While held, three targets 2%, 4% and 6% past the signal close, for 30%, 30% and 42% of that size
//! (the last takes what is left), and a close at the next open once the low (the high for a short) crosses the stop 2% the
//! other way. No costs; starts with 100,000.
//!
//! The rule computes each size once, a `var` inside the function it calls once for each side, so each call keeps its own; its
//! trailing stop is off by default. The reference floors a size the rule states on the decimal it prints as, so a sum of three
//! thirds that comes out a hair under its value loses a lot (0.155 * 3 is 0.46499999999999997, which enters 0.46499), and a
//! target's share of it floors to the lot as well.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::rolling::CrossEither;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::{floor_printed, floor_share};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const CASH: f64 = 50_000.0;
const STOP: f64 = 0.02;
/// `(distance, share of the size)` of each target.
const TARGETS: [(f64, f64); 3] = [(0.02, 0.30), (0.04, 0.30), (0.06, 0.42)];
const NAMES: [[&str; 3]; 2] =
    [["tp1 long", "tp2 long", "tp3 long"], ["tp1 short", "tp2 short", "tp3 short"]];

struct S {
    fast: IndicatorLine,
    slow: IndicatorLine,
    cross: Cross,
    /// The long and the short size, each fixed at that side's first entry.
    sizes: [Option<f64>; 2],
    stop: f64,
    targets: [f64; 3],
    low_cross: CrossEither,
    high_cross: CrossEither,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: IndicatorLine::new("ema", &[10.0]),
        slow: IndicatorLine::new("ema", &[20.0]),
        cross: Cross::new(),
        sizes: [None; 2],
        stop: f64::NAN,
        targets: [f64::NAN; 3],
        low_cross: CrossEither::default(),
        high_cross: CrossEither::default(),
    })
}

/// The rule's `math.round(x, 3)`: half away from zero.
fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

impl S {
    fn enter(&mut self, strategy: &mut Tactics<'_>, bar: &Bar, side: OrderSide) {
        let sizing = strategy.sizing();
        let dir = f64::from(side);
        self.stop = bar.close * (1.0 - dir * STOP);
        self.targets = TARGETS.map(|(distance, _)| bar.close * (1.0 + dir * distance));
        let slot = &mut self.sizes[usize::from(side == Short)];
        let size = *slot.get_or_insert_with(|| {
            let part = round3(sizing.floor(CASH / bar.close) * 30.0 / 100.0);
            floor_printed(part + part + part, sizing.qty_step())
        });
        strategy.entry(if side == Long { "long" } else { "short" }, side).size(size);
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        let held = strategy.position();
        match self.cross.step(fast, slow) {
            Signal::Long if held <= 0.0 => self.enter(strategy, bar, Long),
            Signal::Short if held >= 0.0 => self.enter(strategy, bar, Short),
            _ => {}
        }
        let size = self.sizes[usize::from(held < 0.0)].unwrap_or(0.0);
        let (lots, names) = if held > 0.0 { ("long", NAMES[0]) } else { ("short", NAMES[1]) };
        if held != 0.0 {
            for ((name, &(_, share)), &take) in names.iter().zip(&TARGETS).zip(&self.targets) {
                let size = floor_share(size * share, size, sizing.qty_step());
                strategy.exit(name).from(lots).size(size).take(take);
            }
        }
        let low_crossed = self.low_cross.step(bar.low, self.stop);
        let high_crossed = self.high_cross.step(bar.high, self.stop);
        if (held > 0.0 && low_crossed) || (held < 0.0 && high_crossed) {
            strategy.close(lots);
        }
    }
}
