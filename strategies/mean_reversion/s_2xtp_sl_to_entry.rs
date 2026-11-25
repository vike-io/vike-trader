// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "2xTP, SL to entry" by fpsd4ve, https://www.tradingview.com/script/0Blrq2NA/
// The original's notice: © fpsd4ve
// Changed: rewritten by hand in Rust for vike.

//! Long 25% of the account's worth at the next open when the last bar's low was under the lower Bollinger band (20 bars, 2
//! population deviations) while the smoothed stochastic %K (14, 3) crossed up through its %D (3) under 25; short on the
//! mirror (high over the upper band, %K crossing down over 75). Each reverses the other. Every bar each side states two exits
//! for its entry: half at a target 200 over the average entry, the rest at 500 over, both under one stop. The stop is 200
//! under the close while that side is not held, keeps its last value once held, and moves to the average entry from the bar
//! the position last shrank, until a new signal of that side. A short is the mirror. 0.01% commission; starts with 1,000.
//!
//! The rule reads the position whatever its side: a short's average entry gives the long's targets while it is held, and
//! "the position shrank" is any fall of the signed size, a new short included.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Bollinger, Stochastic};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::helpers::{BarsSince, fewer, known};
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::{floor_share, share_of_worth};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const TP1: f64 = 200.0;
const TP2: f64 = 500.0;
const SL: f64 = 200.0;
const SHARE: f64 = 25.0;

/// What one bar left for the next: the rule's conditions read the last bar.
#[derive(Clone, Copy)]
struct Last {
    low: f64,
    high: f64,
    lower: f64,
    upper: f64,
    k: f64,
    up: bool,
    down: bool,
}

/// One side's state: bars since its condition and since the position moved against it, its stop, its entry's size.
struct Side {
    since_signal: BarsSince,
    since_shrink: BarsSince,
    stop: f64,
    size: f64,
}

impl Side {
    fn new() -> Self {
        Side {
            since_signal: BarsSince::default(),
            since_shrink: BarsSince::default(),
            stop: 0.0,
            size: 0.0,
        }
    }
}

struct S {
    bands: Bollinger,
    stoch: Stochastic,
    cross: Cross,
    last: Option<Last>,
    last_held: f64,
    long: Side,
    short: Side,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        bands: Bollinger::new(20.0, 2.0).expect("valid parameters"),
        stoch: Stochastic::new(14.0, 3.0, 3.0).expect("valid parameters"),
        cross: Cross::new(),
        last: None,
        last_held: 0.0,
        long: Side::new(),
        short: Side::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.01), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let [upper, _, lower] = self.bands.next(bar);
        let [k, d] = self.stoch.next(bar);
        let cross = self.cross.step(k, d);
        let now = Last {
            low: bar.low,
            high: bar.high,
            lower,
            upper,
            k,
            up: matches!(cross, Signal::Long),
            down: matches!(cross, Signal::Short),
        };
        let (long_signal, short_signal) = match self.last.replace(now) {
            Some(l) => {
                (l.low < l.lower && l.up && l.k < 25.0, l.high > l.upper && l.down && l.k > 75.0)
            }
            None => (false, false),
        };

        let held = strategy.position();
        let (in_long, in_short) = (held > 0.0, held < 0.0);
        let long = &mut self.long;
        long.since_shrink.step(held < self.last_held);
        long.since_signal.step(long_signal);
        let short = &mut self.short;
        short.since_shrink.step(held > self.last_held);
        short.since_signal.step(short_signal);
        self.last_held = held;

        let avg = strategy.avg_cost();
        let c = bar.close;
        let long = &mut self.long;
        long.stop = if in_long && fewer(long.since_shrink.0, long.since_signal.0) {
            avg
        } else if in_long {
            long.stop
        } else {
            c - SL
        };
        let short = &mut self.short;
        short.stop = if in_short && fewer(short.since_shrink.0, short.since_signal.0) {
            avg
        } else if in_short {
            short.stop
        } else {
            c + SL
        };

        if long_signal && !in_long {
            self.long.size = share_of_worth(&sizing, SHARE, c);
            strategy.entry("Long", Long).size(self.long.size);
        }
        let first = floor_share(self.long.size * 0.5, self.long.size, sizing.qty_step());
        let rest = self.long.size - first;
        for (name, size, take) in
            [("TP1/SL Long", first, avg + TP1), ("TP2/SL Long", rest, avg + TP2)]
        {
            let exit = strategy.exit(name).from("Long").size(size);
            if let Some(take) = known(take) {
                exit.take(take);
            }
            if let Some(stop) = known(self.long.stop) {
                exit.stop(stop);
            }
        }

        if short_signal && !in_short {
            self.short.size = share_of_worth(&sizing, SHARE, c);
            strategy.entry("Short", Short).size(self.short.size);
        }
        let first = floor_share(self.short.size * 0.5, self.short.size, sizing.qty_step());
        let rest = self.short.size - first;
        for (name, size, take) in
            [("TP1/SL Short", first, avg - TP1), ("TP2/SL Short", rest, avg - TP2)]
        {
            let exit = strategy.exit(name).from("Short").size(size);
            if let Some(take) = known(take) {
                exit.take(take);
            }
            if let Some(stop) = known(self.short.stop) {
                exit.stop(stop);
            }
        }
    }
}
