// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/anomaly-equity-mirror-strategy-equity-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! On each Monday's 00:00 UTC bar, when flat, long the account's whole worth at this close, rounded to 3 decimals. Closed on the first bar
//! after the one the entry filled on, so each size reads back the account's worth. No costs.
//!
//! The rule sets no margin, so 100% applies, and an entry worth more than the account is refused twice over: here by this close when it is
//! placed, and by the engine's cash gate at the open it would fill at. An entry worth exactly the account is taken (`<=`).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_marketdata::Bar;
use vike_model::MS_PER_DAY;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::calendar::clock;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const LOT: &str = "long";
/// Day 0, 1970-01-01, was a Thursday, so a Monday is 4 days into its week of 7.
const MONDAY: i64 = 4;
const THOUSANDTHS: f64 = 1000.0;

struct S;

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S)
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let monday = bar.timestamp.div_euclid(MS_PER_DAY).rem_euclid(7) == MONDAY;
        if monday && clock(bar) == (0, 0) && strategy.position() == 0.0 {
            let worth = strategy.equity();
            let size = (worth / bar.close * THOUSANDTHS).round() / THOUSANDTHS;
            if size * bar.close <= worth {
                strategy.entry(LOT, Long).size(size);
            }
        }
        // A lot's own bar is the one it filled on, and only a later bar flattens it.
        if strategy.lots().first().is_some_and(|lot| bar.timestamp > lot.entry_timestamp) {
            strategy.close(LOT);
        }
    }
}
