// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-closedtrades-risk-introspection-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when the close breaks over the previous bar's 20-bar highest high, short when it breaks under its 20-bar lowest low, reversing
//! each other. Size 1 until a trade has closed, then 0.5 after a losing trade and 2 otherwise, as the last closed trade stood when the
//! signal fired. Anything held closes once it has been more than 1,000 against its entry. The rule's 20% drawdown limit is left out.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const UP: &str = "long";
const DOWN: &str = "short";
const MOST_AGAINST: f64 = 1000.0;

struct S {
    channel: IndicatorLine,
    previous_channel: Vec<f64>,
    /// The open lot's entry and the most it has been against it, in cash.
    worst: (i64, f64),
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        channel: IndicatorLine::new("donchian", &[20.0]),
        previous_channel: vec![f64::NAN; 3],
        worst: (0, 0.0),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        // The `donchian` row's lines are upper, mid, lower.
        let previous = std::mem::replace(&mut self.previous_channel, self.channel.next(bar));
        let size = match strategy.closed_trades().unwrap_or(&[]).last() {
            None => 1.0,
            Some(last) if last.net < 0.0 => 0.5,
            Some(_) => 2.0,
        };
        if bar.close > previous[0] {
            strategy.entry(UP, Long).size(size);
        }
        if bar.close < previous[2] {
            strategy.entry(DOWN, Short).size(size);
        }
        let Some(lot) = strategy.lots().first().cloned() else { return };
        let against = match lot.side {
            Long => (lot.price - bar.low) * lot.qty,
            _ => (bar.high - lot.price) * lot.qty,
        };
        self.worst = if self.worst.0 == lot.entry_timestamp {
            (lot.entry_timestamp, self.worst.1.max(against))
        } else {
            (lot.entry_timestamp, against)
        };
        if self.worst.1 > MOST_AGAINST {
            strategy.close_all();
        }
    }
}
