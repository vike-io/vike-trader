// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-ies-equity-feedback-sizing-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! When flat on the 00:15 UTC bar, long a size that risks 1% of the account's worth x 1.15, x 1.2 when ADX(14) is at least 25, over
//! 2 ATR(14). With the entry, a stop 2 ATR(14) under and a target 4 above this bar's close, so they are live in the entry bar. No costs.
//!
//! An entry worth more than the account is refused: the rule sets no margin, so 100% applies. The entry's worth is read at
//! this bar's close, the price this rule has when it asks.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::calendar::clock;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "long";
const RISK_SHARE: f64 = 1.0 / 100.0;
const QUALITY: f64 = 1.15;
const TREND_ADX: f64 = 25.0;
const TREND_SIZE: f64 = 1.2;
const NEUTRAL_SIZE: f64 = 1.0;
const STOP_ATRS: f64 = 2.0;
const TARGET_ATRS: f64 = 4.0;

struct S {
    adx: IndicatorLine,
    atr: IndicatorLine,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S { adx: IndicatorLine::new("adx", &[14.0]), atr: IndicatorLine::new("atr", &[14.0]) })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        // The `adx` row's lines are adx, +DI, -DI.
        let adx = self.adx.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        if clock(bar) != (0, 15) || strategy.position() != 0.0 {
            return;
        }
        let regime = if adx >= TREND_ADX { TREND_SIZE } else { NEUTRAL_SIZE };
        let worth = strategy.equity();
        let size = worth * RISK_SHARE * regime * QUALITY / (atr * STOP_ATRS);
        if size * bar.close <= worth {
            strategy.entry(NAME, Long).size(size);
            strategy
                .exit("bracket")
                .from(NAME)
                .take(bar.close + atr * TARGET_ATRS)
                .stop(bar.close - atr * STOP_ATRS);
        }
    }
}
