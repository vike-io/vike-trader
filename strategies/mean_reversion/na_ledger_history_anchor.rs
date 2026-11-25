// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/na-ledger-history-anchor-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! The anchor is SMA(12) of the close 31 bars ago, or of this close while that bar does not exist. Long 6% of the account's worth when
//! the close crosses down through 2.3% under the anchor, closed when it crosses up through the anchor. While held, a stop 2.3 ATR(14)
//! under the average cost. 0.05% commission, 1 tick.
//!
//! The downward cross is only asked once the bar 31 back exists, and the upward cross only while the long is held, so that is all
//! each remembers.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use std::collections::VecDeque;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

use vike_indicators::indicators::Sma;
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::Long;
use vike_model::params::Params;

const NAME: &str = "long";
const SHARE: f64 = 0.06;
const LOOKBACK: usize = 31;
const DEVIATION: f64 = 2.3 * 0.01;
const STOP_ATRS: f64 = 2.3;

struct S {
    /// The last `LOOKBACK` closes, oldest first.
    closes: VecDeque<f64>,
    anchor: Sma,
    atr: IndicatorLine,
    under: Cross,
    over: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        closes: VecDeque::with_capacity(LOOKBACK + 1),
        anchor: Sma::new(12.0).expect("a valid period"),
        atr: IndicatorLine::new("atr", &[14.0]),
        under: Cross::new(),
        over: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let delayed = (self.closes.len() == LOOKBACK).then(|| self.closes[0]);
        self.closes.push_back(bar.close);
        if self.closes.len() > LOOKBACK {
            self.closes.pop_front();
        }
        let anchor = self.anchor.next_value(delayed.unwrap_or(bar.close));
        let atr = self.atr.next(bar)[0];
        let threshold = anchor * (1.0 - DEVIATION);
        let enter =
            delayed.is_some() && matches!(self.under.step(bar.close, threshold), Signal::Short);
        if enter {
            strategy
                .entry(NAME, Long)
                .size(sizing.size_for_share(SHARE, sizing.slipped(Long, bar.close)));
        }
        let held = strategy.position() > 0.0;
        if held && matches!(self.over.step(bar.close, anchor), Signal::Long) {
            strategy.close(NAME);
        }
        if held {
            let cut = strategy.avg_cost() - atr * STOP_ATRS;
            strategy.exit("guard").from(NAME).stop(cut);
        }
    }
}
