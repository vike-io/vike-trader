// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-zenith-rci-wpr-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 2 when flat, the rank correlation of the last 17 closes against their order in time crosses up through 18 and Williams
//! %R(22) is above -58. Closed when the rank correlation is below -14, Williams %R is below -82, or the close is more than 2 ATR(14)
//! under the average cost. 0.05% commission, 1 tick.
//!
//! The rank correlation is vike's `rank_correlation` giving tied closes their mean rank (`with_mean_rank_ties`; unset, ties
//! are ranked by age).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::RankCorrelation;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const NAME: &str = "long";
const SIZE: f64 = 2.0;
const WINDOW: f64 = 17.0;
const ENTER_ABOVE: f64 = 18.0;
const WILLIAMS_ABOVE: f64 = -58.0;
const LEAVE_BELOW: f64 = -14.0;
const WILLIAMS_FLOOR: f64 = -82.0;
const MOST_ATRS_AGAINST: f64 = 2.0;

struct S {
    /// Spearman's rho of the last 17 closes against time, in percent, NaN until the window is full.
    rank: RankCorrelation,
    williams: IndicatorLine,
    atr: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        rank: RankCorrelation::new(WINDOW).expect("a valid period").with_mean_rank_ties(true),
        williams: IndicatorLine::new("williams", &[22.0]),
        atr: IndicatorLine::new("atr", &[14.0]),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let rho = self.rank.next_value(bar.close);
        let williams = self.williams.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        let crossed_up = matches!(self.cross.step(rho, ENTER_ABOVE), Signal::Long);
        let enter = crossed_up && williams > WILLIAMS_ABOVE;
        let ended = rho < LEAVE_BELOW || williams < WILLIAMS_FLOOR;
        let held = strategy.position();
        let too_far = held > 0.0 && bar.close < strategy.avg_cost() - atr * MOST_ATRS_AGAINST;
        if held == 0.0 && enter {
            strategy.entry(NAME, Long).size(SIZE);
        } else if held > 0.0 && (ended || too_far) {
            strategy.close(NAME);
        }
    }
}
