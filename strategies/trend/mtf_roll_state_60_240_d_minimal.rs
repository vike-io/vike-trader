// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/mtf-roll-state-60-240-d-minimal-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Two long entries while flat: a closed UTC hour above the last hour with the 4-hour close not lower than the bar before, or the
//! 15-minute close crossing up through the high of the day before the last closed UTC day. The hourly entry also closes on a 4-hour
//! close below the last one, and either entry closes when the 15-minute close is more than 0.5% below that daily high.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::higher::HigherTimeframeBars;
use vike_tactics::signal::{Cross, Signal, SignalRule, SignalSource};

/// Which entry holds the position: the 4-hour exit closes only the hourly one.
#[derive(PartialEq)]
enum Held {
    Flat,
    Hourly,
    Daily,
}

struct S {
    hours: HigherTimeframeBars,
    four_hours: HigherTimeframeBars,
    days: HigherTimeframeBars,
    hour_close: f64,
    four_hour_close: f64,
    last_closed_high: f64,
    level: f64,
    through_level: Cross,
    held: Held,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        hours: HigherTimeframeBars::minutes(60),
        four_hours: HigherTimeframeBars::minutes(240),
        days: HigherTimeframeBars::minutes(1_440),
        hour_close: f64::NAN,
        four_hour_close: f64::NAN,
        last_closed_high: f64::NAN,
        level: f64::NAN,
        through_level: Cross::new(),
        held: Held::Flat,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let (hour_before, four_hour_before) = (self.hour_close, self.four_hour_close);
        if let Some(hour) = self.hours.push(bar) {
            self.hour_close = hour.close;
        }
        if let Some(four_hour) = self.four_hours.push(bar) {
            self.four_hour_close = four_hour.close;
        }
        if let Some(day) = self.days.push(bar) {
            self.level = self.last_closed_high;
            self.last_closed_high = day.high;
        }
        let crossed_up = matches!(self.through_level.step(bar.close, self.level), Signal::Long);

        if self.held == Held::Flat {
            // On a bar where both fire, the entry placed first fills and the single allowed position refuses the second.
            if self.hour_close > hour_before && self.four_hour_close >= four_hour_before {
                self.held = Held::Hourly;
                Signal::Long
            } else if crossed_up {
                self.held = Held::Daily;
                Signal::Long
            } else {
                Signal::Hold
            }
        } else if bar.close < self.level * 0.995
            || (self.held == Held::Hourly && self.four_hour_close < four_hour_before)
        {
            self.held = Held::Flat;
            Signal::Close
        } else {
            Signal::Hold
        }
    }
}
