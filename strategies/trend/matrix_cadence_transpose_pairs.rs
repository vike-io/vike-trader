// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/matrix-cadence-transpose-pairs-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Three horizons, 6, 18 and 54 bars. One is confirmed when the close is above the close that far back and the volume is above its
//! SMA of that length. Long 2 when flat, the confirmed count has just reached 2 and the close is above EMA(43). Closed when none is
//! confirmed, the close is under the EMA, or the close is more than 2.4 ATR(16) under the average cost. The rule holds the votes in
//! a 2 by 3 grid and reads its transpose by horizon, which pairs the same two votes, so no grid is kept. 0.05% commission, 1 tick.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::Sma;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "long";
const SIZE: f64 = 2.0;
const HORIZONS: [usize; 3] = [6, 18, 54];
const ENTER_COUNT: usize = 2;
const MOST_ATRS_AGAINST: f64 = 2.4;

struct S {
    closes: Vec<f64>,
    volume_averages: [Sma; 3],
    trend: IndicatorLine,
    atr: IndicatorLine,
    was_ready: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        closes: Vec::new(),
        volume_averages: HORIZONS.map(|h| Sma::new(h as f64).expect("a valid period")),
        trend: IndicatorLine::new("ema", &[43.0]),
        atr: IndicatorLine::new("atr", &[16.0]),
        was_ready: false,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        self.closes.push(bar.close);
        let mut confirmed = 0;
        for (&horizon, average) in HORIZONS.iter().zip(&mut self.volume_averages) {
            // Every average takes every bar, whatever the price vote says.
            let firm = bar.volume > average.next_value(bar.volume);
            let back = self.closes.len().checked_sub(1 + horizon);
            if firm && back.is_some_and(|at| bar.close > self.closes[at]) {
                confirmed += 1;
            }
        }
        let trend = self.trend.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        let ready = confirmed >= ENTER_COUNT;
        let enter = ready && !self.was_ready && bar.close > trend;
        self.was_ready = ready;
        let ended = confirmed == 0 || bar.close < trend;
        let held = strategy.position();
        let too_far = held > 0.0 && bar.close < strategy.avg_cost() - atr * MOST_ATRS_AGAINST;
        if held == 0.0 && enter {
            strategy.entry(NAME, Long).size(SIZE);
        } else if held > 0.0 && (ended || too_far) {
            strategy.close(NAME);
        }
    }
}
