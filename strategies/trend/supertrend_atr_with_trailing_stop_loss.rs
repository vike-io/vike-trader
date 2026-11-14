// SPDX-License-Identifier: MIT
// Copyright 2019 Mauricio Pimenta | exit490. Licensed under the MIT license; its text, with this notice, is LICENSE-MIT in the strategies folder.
// Derived from "SUPERTREND ATR WITH TRAILING STOP LOSS" by exit490, https://www.tradingview.com/script/L9GkARD2/
// Changed: rewritten by hand in Rust for vike.

//! Long only. A supertrend on the median price with bands 3 true ranges away (ATR(1)), the lower band rising only while the
//! close before is above its last value and the upper falling only while it is below; the trend turns up on a close above
//! the last upper band and down on one below the last lower band, each turn judged only against the band the trend would
//! cross, and a band the last bar lacked read as this bar's (vike's `Supertrend` with `Supertrend::with_previous_bands` and
//! `Supertrend::with_gated_turns`). Long all of the account's worth while the trend is up and
//! no trade is open, with a stop 3% under the signal close. The long is closed, at the next open, once the close is at or
//! under the stop or the trend is down.
//!
//! The stop rides up: on a bar the close is more than 1% above the reference price (the signal close at first), the stop
//! becomes the larger of itself and the reference price times `1 + (pct - 4) / 100`, `pct` that percentage, and the reference
//! price becomes the stop. No costs; starts with 1,000 in a USD account.
//!
//! The header recalculates on order fills (`Rule::after_fill`): after a fill inside a bar the rule runs again on the WHOLE
//! bar, its persistent values (the reference price and the stop) rolled back to the bar's start while the position is what
//! the fills made it, up to three times, each pass's market order filling at that pass's point of the bar, which also sizes
//! it. Then the rule's definitive run follows on the bar's close.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Supertrend;
use vike_indicators::strategy_kit::max;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{RuleDefaults, Tactics};

const NAME: &str = "LONG";
const MULTIPLIER: f64 = 3.0;
const INITIAL_STOP: f64 = -3.0;
/// The passes a bar's fills can make: one at the open, one at the nearer extreme, one at the other.
const PASSES: usize = 3;

/// The orders one run of the rule gives.
struct Orders {
    close: bool,
    open: bool,
}

#[derive(Clone)]
struct Rule {
    supertrend: Supertrend,
    updated_entry: f64,
    stop: f64,
}

impl Rule {
    fn new() -> Self {
        Rule {
            supertrend: Supertrend::new(1.0, MULTIPLIER)
                .expect("valid parameters")
                .with_previous_bands(true)
                .with_gated_turns(true),
            updated_entry: 0.0,
            stop: 0.0,
        }
    }

    /// One run of the rule on `bar`, with `open` whether a trade is open now, which the rule's values then keep.
    fn step(&mut self, bar: &Bar, open: bool) -> Orders {
        let direction = self.supertrend.next(bar)[1];

        let close = open && (bar.close <= self.stop || direction == -1.0);
        if close {
            (self.updated_entry, self.stop) = (0.0, 0.0);
        }
        let start = !open && direction == 1.0;
        if start {
            self.updated_entry = bar.close;
            self.stop = bar.close + bar.close * INITIAL_STOP / 100.0;
        }
        let pct = (bar.close - self.updated_entry) / self.updated_entry * 100.0;
        if open && pct > 1.0 {
            let percent = INITIAL_STOP + pct - 1.0;
            let next = self.updated_entry + (self.updated_entry * percent) / 100.0;
            self.stop = max(self.stop, next);
            self.updated_entry = self.stop;
        }
        Orders { close, open: start }
    }
}

impl Orders {
    /// The calls in the rule's order, an entry sized at `price`, the close or the point of the bar the run stands at.
    fn place(&self, strategy: &mut Tactics<'_>, price: f64) {
        let sizing = strategy.sizing();
        if self.close {
            strategy.close(NAME);
        }
        if self.open {
            strategy.entry(NAME, Long).size(share_of_worth(&sizing, 100.0, price));
        }
    }
}

struct S {
    rule: Rule,
    /// The passes this bar's fills have made so far.
    passes: usize,
}

pub fn rule(_params: &Params) -> Box<dyn vike_tactics::Rule + Send> {
    Box::new(S { rule: Rule::new(), passes: 0 })
}

impl vike_tactics::Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        self.passes = 0;
        let open = strategy.position() != 0.0;
        self.rule.step(bar, open).place(strategy, bar.close);
    }

    fn after_fill(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        if self.passes == PASSES {
            return;
        }
        // The point of the bar this pass stands at: the open, the extreme nearest it, the other extreme.
        let price = bar.path_points()[self.passes];
        self.passes += 1;
        let open = strategy.position() != 0.0;
        self.rule.clone().step(bar, open).place(strategy, price);
    }
}
