// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Dynamic RSI Momentum" by QuantNomad, https://www.tradingview.com/script/08eSyDlq/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2022-12-31, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! An RSI whose length moves with the 10-bar correlation of the close against its 10-bar momentum: `20 + round((1 - corr) *
//! 15)`, 20 while the correlation is not known. Long half the account's worth when it crosses up through 30, short half when
//! it crosses down through 70, each reversing the other, filled at the signal bar's close. No costs; starts with 1,000,000.
//!
//! The rule writes its own RSI: each of the up and down moves (0 on the first bar, which has no move) is averaged by a Wilder
//! average of the bar's length, seeded by the plain mean of the last that-many moves on the first bar it has them, and from
//! then on `x / len + (1 - 1 / len) * previous` with each bar's own length: vike's `Smma` in that form
//! (`Smma::with_alpha_steps`) fed each bar's length (`Smma::next_value_with_period`).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use vike_tactics::helpers::nan_to_zero;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

use vike_indicators::indicators::Smma;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;

const MOMENTUM: usize = 10;
const MIN_LEN: f64 = 20.0;
const MAX_LEN: f64 = 50.0;
const OVERSOLD: f64 = 30.0;
const OVERBOUGHT: f64 = 70.0;

/// The rule's Wilder average (`rmaFun`), its length given on each bar, at most `MAX_LEN`.
fn var_rma() -> Smma {
    Smma::new(MAX_LEN).expect("a valid length").with_alpha_steps(true)
}

/// Pearson's correlation of the last `MOMENTUM` pairs, NaN until that many have come.
fn correlation(pairs: &VecDeque<(f64, f64)>) -> f64 {
    if pairs.len() < MOMENTUM {
        return f64::NAN;
    }
    let n = pairs.len() as f64;
    let (mx, my) = pairs.iter().fold((0.0, 0.0), |(a, b), &(x, y)| (a + x / n, b + y / n));
    let (mut sxy, mut sxx, mut syy) = (0.0, 0.0, 0.0);
    for &(x, y) in pairs {
        sxy += (x - mx) * (y - my);
        sxx += (x - mx) * (x - mx);
        syy += (y - my) * (y - my);
    }
    sxy / (sxx * syy).sqrt()
}

struct S {
    closes: VecDeque<f64>,
    pairs: VecDeque<(f64, f64)>,
    up: Smma,
    down: Smma,
    long: Cross,
    short: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        closes: VecDeque::new(),
        pairs: VecDeque::new(),
        up: var_rma(),
        down: var_rma(),
        long: Cross::new(),
        short: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let previous = self.closes.back().copied().unwrap_or(f64::NAN);
        self.closes.push_back(bar.close);
        if self.closes.len() > MOMENTUM + 1 {
            self.closes.pop_front();
        }
        if self.closes.len() == MOMENTUM + 1 {
            self.pairs.push_back((bar.close, bar.close - self.closes[0]));
            if self.pairs.len() > MOMENTUM {
                self.pairs.pop_front();
            }
        }
        let corr = correlation(&self.pairs);
        let corr = if (-1.0..=1.0).contains(&corr) { corr } else { f64::NAN };
        let step = ((1.0 - corr) * (MAX_LEN - MIN_LEN) / 2.0).round();
        let len = MIN_LEN + nan_to_zero(step);

        let change = bar.close - previous;
        let up = self.up.next_value_with_period(if change > 0.0 { change } else { 0.0 }, len);
        let down = self.down.next_value_with_period(if -change > 0.0 { -change } else { 0.0 }, len);
        let rsi = 100.0 - 100.0 / (1.0 + up / down);

        let long = matches!(self.long.step(rsi, OVERSOLD), Signal::Long);
        let short = matches!(self.short.step(rsi, OVERBOUGHT), Signal::Short);
        if rsi.is_nan() {
            return;
        }
        if long {
            strategy.entry("long", Long).size(share_of_worth(&sizing, 50.0, bar.close));
        }
        if short {
            strategy.entry("short", Short).size(share_of_worth(&sizing, 50.0, bar.close));
        }
    }
}
