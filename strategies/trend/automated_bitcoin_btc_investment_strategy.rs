// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Automated Bitcoin (BTC) Investment Strategy" by WunderTrading, https://www.tradingview.com/script/0mCr8Nfv/
// The original's notice: © Wunderbit Trading
// Changed: rewritten by hand in Rust for vike.

//! Long only, all of the account's worth, when TEMA(25) of the close crosses up through the 100-bar linear regression of the
//! close and the close is above the highest trail of the last 50 bars. While held, two targets on the average price, 20% of
//! the entry each, at 15% and at 30% up. Closed at the next open when the close is under that highest trail, when the
//! regression crosses back up through TEMA, or when the close is 5% under the average price. 0.1% commission; starts with
//! 5,000.
//!
//! The trail is the rule's own: with `stop = 3.5 * ATR(8)` (the ATR seeded with the first true range, then
//! `atr + (tr - atr) / 8`, the first bar having none: vike's `Atr::with_seed_first`) and the last trail read as 0 while it
//! has none, the trail is
//! `min(last, close + stop)` when this close and the last are both under it, otherwise `close - stop` when this close is above
//! it and `close + stop` when not. A target's share of the entry floors to the quantity step.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Atr, Donchian, Linearreg, Tema};
use vike_indicators::strategy_kit::min;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::helpers::nan_to_zero;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const NAME: &str = "long";
const MULTIPLIER: f64 = 3.5;
const ATR_PERIOD: f64 = 8.0;
const SHARE: f64 = 0.20;
const FIRST_TARGET: f64 = 0.15;
const SECOND_TARGET: f64 = 0.30;
const STOP: f64 = 0.05;

struct S {
    tema: Tema,
    regression: Linearreg,
    cross: Cross,
    atr: Atr,
    trail: f64,
    trail_high: Donchian,
    last_close: f64,
    entered: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        tema: Tema::new(25.0).expect("a valid length"),
        regression: Linearreg::new(100.0).expect("a valid length"),
        cross: Cross::new(),
        atr: Atr::new(ATR_PERIOD).expect("a valid length").with_seed_first(true),
        trail: f64::NAN,
        trail_high: Donchian::new(50.0).expect("a valid length"),
        last_close: f64::NAN,
        entered: 0.0,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.1), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let tema = self.tema.next(bar);
        let regression = self.regression.next_value(bar.close);
        let signal = self.cross.step(tema, regression);

        let last_close = std::mem::replace(&mut self.last_close, bar.close);
        let stop = MULTIPLIER * self.atr.next(bar);
        let last = nan_to_zero(self.trail);
        let close = bar.close;
        // Not vike's `AtrTrailingStop`: this trail never ratchets up, every close above it resets it to `close - stop`.
        self.trail = if close < last && last_close < last {
            min(last, close + stop)
        } else if close > last {
            close - stop
        } else {
            close + stop
        };
        let trail_high = self.trail_high.next_hl(self.trail, self.trail)[0];

        let entry = matches!(signal, Signal::Long) && trail_high < close;
        let cost = strategy.avg_cost();
        let exit =
            close < trail_high || matches!(signal, Signal::Short) || close < cost * (1.0 - STOP);

        let held = strategy.position();
        if held == 0.0 {
            self.entered = 0.0;
        } else if self.entered == 0.0 {
            self.entered = held;
        }
        if entry {
            strategy.entry(NAME, Long).size(share_of_worth(&sizing, 100.0, close));
        }
        if held > 0.0 {
            let size = sizing.floor(self.entered * SHARE);
            let take = cost * (1.0 + FIRST_TARGET);
            strategy.exit("TP1").from(NAME).size(size).take(take);
            let take = cost * (1.0 + SECOND_TARGET);
            strategy.exit("TP2").from(NAME).size(size).take(take);
        }
        if exit {
            strategy.close(NAME);
        }
    }
}
