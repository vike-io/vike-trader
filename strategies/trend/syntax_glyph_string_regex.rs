// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/syntax-glyph-string-regex-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 2 when flat and EMA(18) crosses up through EMA(54) with RSI(14) above a level, closed when it crosses down, RSI(14) is
//! under 44, or the close is more than some ATR(16) under the average cost. A text label picks the two settings: it must name a
//! trend rule at all, "trend:confirmed" makes the level 57 rather than 51, and "risk:guarded" keeps the ATR multiple at 2.2 rather
//! than 1.4 times that. 0.05% commission, 1 tick.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "long";
const SIZE: f64 = 2.0;
const LABEL: &str = "Trend:Confirmed|Risk:Guarded";
const CONFIRMED_LEVEL: f64 = 57.0;
const PLAIN_LEVEL: f64 = 51.0;
const LEAVE_BELOW: f64 = 44.0;
const GUARDED_ATRS: f64 = 2.2;
const LOOSE_RISK_FACTOR: f64 = 1.4;

/// The rule's one real pattern, without a regex crate: "trend:" followed by at least one letter a to z, anywhere in the text.
fn names_a_trend_rule(lowered: &str) -> bool {
    lowered
        .match_indices("trend:")
        .any(|(at, word)| lowered[at + word.len()..].starts_with(|c: char| c.is_ascii_lowercase()))
}

struct S {
    has_trend_rule: bool,
    enter_above: f64,
    most_atrs_against: f64,
    fast: IndicatorLine,
    slow: IndicatorLine,
    rsi: IndicatorLine,
    atr: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    assert!(
        names_a_trend_rule("x|trend:a")
            && !names_a_trend_rule("trend:")
            && !names_a_trend_rule("trend:1")
    );
    let lowered = LABEL.to_lowercase();
    let guarded = lowered.contains("risk:guarded");
    Box::new(S {
        has_trend_rule: names_a_trend_rule(&lowered),
        enter_above: if lowered.contains("trend:confirmed") {
            CONFIRMED_LEVEL
        } else {
            PLAIN_LEVEL
        },
        most_atrs_against: if guarded { GUARDED_ATRS } else { GUARDED_ATRS * LOOSE_RISK_FACTOR },
        fast: IndicatorLine::new("ema", &[18.0]),
        slow: IndicatorLine::new("ema", &[54.0]),
        rsi: IndicatorLine::new("rsi", &[14.0]),
        atr: IndicatorLine::new("atr", &[16.0]),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        let rsi = self.rsi.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        let crossed = self.cross.step(fast, slow);
        let enter =
            self.has_trend_rule && matches!(crossed, Signal::Long) && rsi > self.enter_above;
        let ended = matches!(crossed, Signal::Short) || rsi < LEAVE_BELOW;
        let held = strategy.position();
        let too_far = held > 0.0 && bar.close < strategy.avg_cost() - atr * self.most_atrs_against;
        if held == 0.0 && enter {
            strategy.entry(NAME, Long).size(SIZE);
        } else if held > 0.0 && (ended || too_far) {
            strategy.close(NAME);
        }
    }
}
