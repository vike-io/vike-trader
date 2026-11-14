// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/array-atlas-momentum-rotation-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 7% of the account's worth when the close's rank among the 55 closes before it crosses up through 78, the mean of the last 8
//! one-bar returns is above 0 and RSI(14) is above 54. Closed when the rank crosses down through 46, the mean is under 0 or RSI is
//! under 44. While held, a stop 2.4 ATR(17) under the average cost, moved every bar. 0.05% commission, 1 tick. Starts with 100,000.
//!
//! The rank is vike's `PercentRank`: the percent of the previous `RANK_LENGTH` closes less than or equal to this one.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::{PercentRank, Sma};
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{CrossBands, Signal};
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const NAME: &str = "long";
const SHARE: f64 = 0.07;
const SAMPLES: f64 = 8.0;
const RANK_LENGTH: f64 = 55.0;
const ENTRY_RANK: f64 = 78.0;
const EXIT_RANK: f64 = 46.0;
const ENTRY_RSI: f64 = 54.0;
const EXIT_RSI: f64 = 44.0;
const STOP_ATRS: f64 = 2.4;

struct S {
    /// The mean of the returns, its window summed afresh.
    returns: Sma,
    /// The last close, NaN before the first.
    last_close: f64,
    rank: PercentRank,
    rsi: IndicatorLine,
    atr: IndicatorLine,
    rank_turns: CrossBands,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        returns: Sma::new(SAMPLES).expect("a valid period").with_fresh_sum(true),
        last_close: f64::NAN,
        rank: PercentRank::new(RANK_LENGTH).expect("a valid length"),
        rsi: IndicatorLine::new("rsi", &[14.0]),
        atr: IndicatorLine::new("atr", &[17.0]),
        rank_turns: CrossBands::new(ENTRY_RANK, EXIT_RANK),
    })
}

impl S {
    /// The mean of the last `SAMPLES` one-bar returns in percent, NaN until that many are in. The first bar's return is 0.
    fn impulse(&mut self, close: f64) -> f64 {
        let before = std::mem::replace(&mut self.last_close, close);
        let change = if before.is_nan() { 0.0 } else { 100.0 * (close / before - 1.0) };
        self.returns.next_value(change)
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let impulse = self.impulse(bar.close);
        let rank = self.rank.next_value(bar.close);
        let rsi = self.rsi.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        let turn = self.rank_turns.step(rank);
        if matches!(turn, Signal::Long) && impulse > 0.0 && rsi > ENTRY_RSI {
            strategy
                .entry(NAME, Long)
                .size(sizing.size_for_share(SHARE, sizing.slipped(Long, bar.close)));
        }
        if strategy.position() > 0.0 {
            if matches!(turn, Signal::Short) || impulse < 0.0 || rsi < EXIT_RSI {
                strategy.close(NAME);
            }
            let cut = strategy.avg_cost() - atr * STOP_ATRS;
            strategy.exit("stop").from(NAME).stop(cut);
        }
    }
}
