// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Super Guppy Strategy" by StanleyBostich, https://www.tradingview.com/script/vPStjyWz/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2020-09-21, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! The slow ribbon is fifteen EMAs of the close, 24 to 66 bars in steps of 3: green when each is above the next longer one,
//! red when each is below it, grey otherwise. A turn to green from grey, or to grey from red, is a long signal unless the
//! last signal was long; a turn to grey from green, or to red from grey, is a short signal unless the last was short. A
//! long signal goes long all of the account's worth, reversing a short; a short signal closes a long, then goes short all
//! of it. No costs; starts with 100,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Ema;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

#[derive(Clone, Copy, PartialEq)]
enum Colour {
    Green,
    Red,
    Grey,
}

struct S {
    ribbon: Vec<Ema>,
    last: Option<Colour>,
    long: bool,
    short: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        ribbon: (0..15).map(|i| Ema::new(f64::from(24 + 3 * i)).expect("a valid length")).collect(),
        last: None,
        long: false,
        short: false,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let lines: Vec<f64> = self.ribbon.iter_mut().map(|e| e.next(bar)).collect();
        let colour = if lines.windows(2).all(|w| w[0] > w[1]) {
            Colour::Green
        } else if lines.windows(2).all(|w| w[0] < w[1]) {
            Colour::Red
        } else {
            Colour::Grey
        };
        let turn = |from: Colour, to: Colour| self.last == Some(from) && colour == to;
        let long =
            !self.long && (turn(Colour::Grey, Colour::Green) || turn(Colour::Red, Colour::Grey));
        let short =
            !self.short && (turn(Colour::Green, Colour::Grey) || turn(Colour::Grey, Colour::Red));
        self.last = Some(colour);
        if long {
            (self.long, self.short) = (true, false);
            strategy.entry("long", Long).size(share_of_worth(&sizing, 100.0, bar.close));
        }
        if short {
            (self.long, self.short) = (false, true);
            strategy.close("long");
            strategy.entry("short", Short).size(share_of_worth(&sizing, 100.0, bar.close));
        }
    }
}
