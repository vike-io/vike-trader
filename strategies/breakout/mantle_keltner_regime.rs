// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-mantle-keltner-regime-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 2 when flat and the close crosses up through a channel's upper band with the channel's centre above EMA(57). Closed when
//! the close is below the centre, or more than 0.65 channel widths under the average cost. 0.05% commission, 1 tick.
//!
//! The channel is vike's `keltner` with its band from an EMA of the true range (`with_range_ema`; unset, Wilder's average):
//! EMA(24) of the close, 1.7 EMA(24)s of the true range either side.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::{Ema, Keltner};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "long";
const SIZE: f64 = 2.0;
const CHANNEL_LENGTH: f64 = 24.0;
const BAND_RANGES: f64 = 1.7;
const TREND_LENGTH: f64 = 57.0;
const MOST_WIDTHS_AGAINST: f64 = 0.65;

struct S {
    channel: Keltner,
    trend: Ema,
    through_upper: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        channel: Keltner::new(CHANNEL_LENGTH, CHANNEL_LENGTH, BAND_RANGES)
            .expect("valid lengths")
            .with_range_ema(true),
        trend: Ema::new(TREND_LENGTH).expect("a valid period"),
        through_upper: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let [upper, centre, lower] = self.channel.next(bar);
        let trend = self.trend.next_value(bar.close);

        let crossed_up = matches!(self.through_upper.step(bar.close, upper), Signal::Long);
        let held = strategy.position();
        let too_far =
            held > 0.0 && bar.close < strategy.avg_cost() - (upper - lower) * MOST_WIDTHS_AGAINST;
        if held == 0.0 && crossed_up && centre > trend {
            strategy.entry(NAME, Long).size(SIZE);
        } else if held > 0.0 && (bar.close < centre || too_far) {
            strategy.close(NAME);
        }
    }
}
