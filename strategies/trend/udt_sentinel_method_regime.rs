// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/udt-sentinel-method-regime-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 6% of the account's worth when RSI(15) crosses up through 54 with the close above EMA(36). Closed when RSI crosses down through
//! 48 or the close is under the EMA. While held, a stop 2.1 ATR(14) under the average cost, moved every bar. 0.05% commission, 1 tick.
//! Starts with 100,000.
//!
//! The regime the rule also asks for, RSI above 50 and at least 54, always holds on a bar where RSI has just crossed above 54. It is
//! kept as stated.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{CrossBands, Signal};
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const NAME: &str = "long";
const SHARE: f64 = 0.06;
const ENOUGH_STRENGTH: f64 = 54.0;
const BULLISH_STRENGTH: f64 = 50.0;
const WEAK_STRENGTH: f64 = 48.0;
const STOP_ATRS: f64 = 2.1;

/// The market's regime as of one bar.
struct Regime {
    center: f64,
    strength: f64,
    bullish: bool,
}

impl Regime {
    fn of(close: f64, center: f64, strength: f64) -> Self {
        Regime { center, strength, bullish: close > center && strength > BULLISH_STRENGTH }
    }

    fn permits_long(&self) -> bool {
        self.bullish && self.strength >= ENOUGH_STRENGTH
    }
}

struct S {
    center: IndicatorLine,
    strength: IndicatorLine,
    atr: IndicatorLine,
    turns: CrossBands,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        center: IndicatorLine::new("ema", &[36.0]),
        strength: IndicatorLine::new("rsi", &[15.0]),
        atr: IndicatorLine::new("atr", &[14.0]),
        turns: CrossBands::new(ENOUGH_STRENGTH, WEAK_STRENGTH),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let center = self.center.next(bar)[0];
        let strength = self.strength.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        let regime = Regime::of(bar.close, center, strength);
        let turn = self.turns.step(strength);
        if matches!(turn, Signal::Long) && regime.permits_long() {
            strategy
                .entry(NAME, Long)
                .size(sizing.size_for_share(SHARE, sizing.slipped(Long, bar.close)));
        }
        if strategy.position() > 0.0 {
            if matches!(turn, Signal::Short) || bar.close < regime.center {
                strategy.close(NAME);
            }
            let cut = strategy.avg_cost() - atr * STOP_ATRS;
            strategy.exit("stop").from(NAME).stop(cut);
        }
    }
}
