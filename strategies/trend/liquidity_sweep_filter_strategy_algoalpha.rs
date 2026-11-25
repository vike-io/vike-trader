// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from the script by PineIndicators, https://www.tradingview.com/script/gx7267BR/
// The original's notice: © AlgoAlpha X PineIndicators
// Changed: rewritten by hand in Rust for vike.

//! A band around SMA(12) of EMA(12) of the close, two EMA(36)s of the close's distance from that middle wide. A close over the
//! band turns the trend up, a close under it turns the trend down. Long only: all of the account when the trend turns up,
//! closed when it turns down, filling at the signal bar's close. 0.05% commission, 2 ticks of slippage; starts with 100,000.
//!
//! The rest of the rule (liquidity sweeps, a volume profile, their labels and alerts) only draws.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! fill = "close"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Ema, Sma};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "Long";
const LEN: f64 = 12.0;
const MULTIPLIER: f64 = 2.0;

struct S {
    ema: Ema,
    basis: Sma,
    deviation: Ema,
    trend: f64,
    previous_trend: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        ema: Ema::new(LEN).expect("a valid length"),
        basis: Sma::new(LEN).expect("a valid length"),
        deviation: Ema::new(LEN * 3.0).expect("a valid length").with_skip_nan(true),
        trend: 0.0,
        previous_trend: f64::NAN,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let ema = self.ema.next_value(bar.close);
        let basis = if ema.is_nan() { f64::NAN } else { self.basis.next_value(ema) };
        let deviation = self.deviation.next_value((bar.close - basis).abs()) * MULTIPLIER;
        if bar.close > basis + deviation {
            self.trend = 1.0;
        }
        if bar.close < basis - deviation {
            self.trend = -1.0;
        }
        let (trend, previous) = (self.trend, self.previous_trend);
        self.previous_trend = trend;
        if trend > 0.0 && previous <= 0.0 && strategy.position() <= 0.0 {
            strategy.entry(NAME, Long).size(share_of_worth(
                &sizing,
                100.0,
                sizing.slipped(Long, bar.close),
            ));
        }
        if trend < 0.0 && previous >= 0.0 && strategy.position() > 0.0 {
            strategy.close(NAME);
        }
    }
}
