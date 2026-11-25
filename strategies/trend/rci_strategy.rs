// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "RCI Strategy [PineIndicators]" by PineIndicators, https://www.tradingview.com/script/BsqKKqyq/
// The original's notice: © PineIndicators
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2025-05-13, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! The rank correlation of the last 10 closes against their order in time, and its 14-bar simple average. When the index
//! crosses up through the average the rule goes long with all of the account's worth and closes the short; when it crosses
//! down it goes short and closes the long. 0.01% commission, 2 ticks of slippage. The header states neither a capital nor a
//! margin: it starts with 100,000 and a position takes 100% of its value in margin. **Does not reproduce its reference**
//! (5738 trades, net -77588.953125 on BTCUSDT 4h, 2017-08-17..2026-10-02): this file gives 5935 / -68056.018335, 197 trades
//! more and the net 9532.93 over: not in the margin call (every cut equals the reference's rules), but on 2017-08-27 08:00,
//! where the margin check refuses the long's reversing entry and the reference also cancels the same-bar close of the short,
//! which vike fills, and on 2017-10-10 08:00, where the index equals its average (900/11) and vike's running-sum average puts
//! it one ulp under, so only the reference sees the cross; the owner accepted those numbers on 2026-11-15.
//!
//! The index gives tied closes their average rank (`with_mean_rank_ties`). The labels, boxes and lines it draws are left out.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{RankCorrelation, Sma};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const LENGTH: f64 = 10.0;
const AVERAGE_LENGTH: f64 = 14.0;

struct S {
    index: RankCorrelation,
    average: Sma,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        index: RankCorrelation::new(LENGTH).expect("a valid period").with_mean_rank_ties(true),
        average: Sma::new(AVERAGE_LENGTH).expect("a valid period"),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.01), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let index = self.index.next_value(bar.close);
        // The average starts on the index's first value.
        let average = if index.is_nan() { f64::NAN } else { self.average.next_value(index) };
        let sizing = strategy.sizing();
        match self.cross.step(index, average) {
            Signal::Long => {
                strategy.entry("L", Long).size(share_of_worth(
                    &sizing,
                    100.0,
                    sizing.slipped(Long, bar.close),
                ));
                strategy.close("S");
            }
            Signal::Short => {
                strategy.entry("S", Short).size(share_of_worth(
                    &sizing,
                    100.0,
                    sizing.slipped(Short, bar.close),
                ));
                strategy.close("L");
            }
            _ => {}
        }
    }
}
