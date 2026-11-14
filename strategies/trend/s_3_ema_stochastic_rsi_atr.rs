// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "3 EMA + Stochastic RSI + ATR" by vaidab, https://www.tradingview.com/script/IhxCXn6n/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2021-06-22, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Three EMAs of the close (8, 14, 50). Not long, long 10% of the account's worth when they stand in order fast above medium
//! above slow and the close is above the fast one; not short, short 10% on the mirror. Each entry sets a target 4 ATR(14)
//! and a stop 1 ATR from the signal close (for a short, the other way), and the exit rests at the levels last set, from the
//! entry on. Orders fill at the close of the bar that places them. 0.1% commission and 2 ticks of slippage; starts with
//! 1,000 in a USD account.
//!
//! The rule's date range (1900 to 2040) holds on every bar here, and its stochastic RSI is switched off.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Atr, Ema};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::helpers::known;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const SHARE: f64 = 10.0;

struct S {
    fast: Ema,
    medium: Ema,
    slow: Ema,
    atr: Atr,
    /// The target and the stop last set.
    levels: (f64, f64),
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: Ema::new(8.0).expect("a valid length"),
        medium: Ema::new(14.0).expect("a valid length"),
        slow: Ema::new(50.0).expect("a valid length"),
        atr: Atr::new(14.0).expect("a valid length"),
        levels: (f64::NAN, f64::NAN),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.1), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let fast = self.fast.next(bar);
        let medium = self.medium.next(bar);
        let slow = self.slow.next(bar);
        let atr = self.atr.next(bar);
        let held = strategy.position();
        let long = fast > medium && medium > slow && bar.close > fast && held <= 0.0;
        let short = fast < medium && medium < slow && bar.close < fast && held >= 0.0;
        if long {
            self.levels = (bar.close + 4.0 * atr, bar.close - atr);
            strategy.entry("3ESRA", Long).size(share_of_worth(&sizing, SHARE, bar.close));
        } else if short {
            self.levels = (bar.close - 4.0 * atr, bar.close + atr);
            strategy.entry("3ESRA", Short).size(share_of_worth(&sizing, SHARE, bar.close));
        }
        let (take, cut) = self.levels;
        let exit = strategy.exit("TP-SL").from("3ESRA");
        if let Some(take) = known(take) {
            exit.take(take);
        }
        if let Some(cut) = known(cut) {
            exit.stop(cut);
        }
    }
}
