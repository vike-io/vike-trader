// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/volume-tide-vwap-mfi-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 6% of the account's worth when the typical price crosses up through its daily VWAP with MFI(17) above 56. While held, closed
//! when the typical price crosses down through that VWAP or MFI(17) is below 43, and a stop 2.1 ATR(18) under the average cost.
//! 0.05% commission, 1 tick. Starts with 100,000.
//!
//! The VWAP is vike's `vwap` weighting the typical price divided by 3 (`VwapSource::TypicalDivided`): the row's default multiplies
//! by a third, and on a day's first bar the VWAP equals the source but for rounding, so rounding decides a cross.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::{Vwap, VwapSource};
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "long";
const SHARE: f64 = 0.06;
const ENTRY_FLOW: f64 = 56.0;
const EXIT_FLOW: f64 = 43.0;
const STOP_ATRS: f64 = 2.1;

struct S {
    vwap: Vwap,
    flow: IndicatorLine,
    atr: IndicatorLine,
    rising: Cross,
    falling: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        vwap: Vwap::default().with_source(VwapSource::TypicalDivided),
        flow: IndicatorLine::new("mfi", &[17.0]),
        atr: IndicatorLine::new("atr", &[18.0]),
        rising: Cross::new(),
        falling: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let source = (bar.high + bar.low + bar.close) / 3.0;
        let vwap = self.vwap.next(bar);
        let flow = self.flow.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        let up = matches!(self.rising.step(source, vwap), Signal::Long);
        if up && flow > ENTRY_FLOW {
            let size = sizing.size_for_share(SHARE, sizing.slipped(Long, bar.close));
            strategy.entry(NAME, Long).size(size);
        }
        if strategy.position() > 0.0 {
            // This cross sits behind the position test: it only remembers the bars spent holding.
            let down = matches!(self.falling.step(source, vwap), Signal::Short);
            if down || flow < EXIT_FLOW {
                strategy.close(NAME);
            }
            let cut = strategy.avg_cost() - atr * STOP_ATRS;
            strategy.exit("stop").from(NAME).stop(cut);
        }
    }
}
