// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/udt-courier-return-method-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! An envelope built fresh on every bar: EMA(37) as its middle and ATR(16) as its width. Long 2 when flat and the close crosses up
//! through the middle plus 1.15 widths, closed when the close is under the middle or more than 2.4 widths under the average cost.
//! The rule also checks the envelope was built on this bar, which is always so. 0.05% commission, 1 tick.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const NAME: &str = "long";
const SIZE: f64 = 2.0;
const ENTRY_WIDTHS: f64 = 1.15;
const MOST_WIDTHS_AGAINST: f64 = 2.4;

struct Envelope {
    middle: f64,
    width: f64,
    built_ms: i64,
}

impl Envelope {
    fn gate(&self, widths: f64) -> f64 {
        self.middle + self.width * widths
    }
}

struct S {
    middle: IndicatorLine,
    width: IndicatorLine,
    cross: Cross,
}

impl S {
    fn envelope(&mut self, bar: &Bar) -> Envelope {
        Envelope {
            middle: self.middle.next(bar)[0],
            width: self.width.next(bar)[0],
            built_ms: bar.timestamp,
        }
    }
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        middle: IndicatorLine::new("ema", &[37.0]),
        width: IndicatorLine::new("atr", &[16.0]),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let envelope = self.envelope(bar);
        let crossed_up =
            matches!(self.cross.step(bar.close, envelope.gate(ENTRY_WIDTHS)), Signal::Long);
        let enter = bar.timestamp == envelope.built_ms && crossed_up;
        let held = strategy.position();
        let too_far =
            held > 0.0 && bar.close < strategy.avg_cost() - envelope.width * MOST_WIDTHS_AGAINST;
        if held == 0.0 && enter {
            strategy.entry(NAME, Long).size(SIZE);
        } else if held > 0.0 && (bar.close < envelope.middle || too_far) {
            strategy.close(NAME);
        }
    }
}
