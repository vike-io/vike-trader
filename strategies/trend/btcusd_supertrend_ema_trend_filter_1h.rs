// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "BTCUSD Supertrend + EMA Trend Filter (1H)" by blitz_locked, https://www.tradingview.com/script/NgSIfKQx/
// The original's notice: © blitz_locked
// Changed: rewritten by hand in Rust for vike.

//! Supertrend(10, 1.8) with an EMA(200) filter. On the bar the trend turns up: long 25% of the account's worth if the close is
//! over the EMA, and any short closed; on the bar it turns down: short 25% if the close is under the EMA, and any long closed. An
//! entry reverses the other side. The stop and target are switched off. 0.075% commission, 1 tick; starts with 10,000.
//!
//! The rule calls its close after its entry, so a reversal fills first and the close then finds nothing of its side to close. A
//! market entry is sized at this close moved the adverse way by the slippage.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Supertrend;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const LONG_NAME: &str = "Long";
const SHORT_NAME: &str = "Short";
const SHARE: f64 = 25.0;

struct S {
    supertrend: Supertrend,
    ema: IndicatorLine,
    previous_direction: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        supertrend: Supertrend::new(10.0, 1.8).expect("valid parameters").with_down_start(true),
        ema: IndicatorLine::new("ema", &[200.0]),
        previous_direction: f64::NAN,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.075), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        // The rule's direction: 1 down, -1 up.
        let direction = -self.supertrend.next(bar)[1];
        let ema = self.ema.next(bar)[0];
        let change = direction - std::mem::replace(&mut self.previous_direction, direction);
        let (bull_flip, bear_flip) = (change < 0.0, change > 0.0);
        if bull_flip && bar.close > ema {
            let size = share_of_worth(&sizing, SHARE, sizing.slipped(Long, bar.close));
            strategy.entry(LONG_NAME, Long).size(size);
        }
        if bear_flip && bar.close < ema {
            let size = share_of_worth(&sizing, SHARE, sizing.slipped(Short, bar.close));
            strategy.entry(SHORT_NAME, Short).size(size);
        }
        if bear_flip {
            strategy.close(LONG_NAME);
        }
        if bull_flip {
            strategy.close(SHORT_NAME);
        }
    }
}
