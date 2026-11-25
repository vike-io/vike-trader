//! What a strategy's hooks FAILED with over a run ([`StrategyFailures`], `Strategy::failures`): a `.rs`
//! file's hook that panicked or was skipped (counted on the host, `vike_rust_strategy::host`), a `.rhai`
//! script's hook that errored (`vike_rhai`'s fail-safe). The engine copies it into the run's record and the
//! report carries it, so a strategy that failed on every bar never reads as a normal report.
//!
//! Bounded whatever the run's length: a count, the strategy clock of the first failure, the first
//! [`StrategyFailures::MESSAGES_KEPT`] distinct messages, and whether the strategy stopped running its
//! hooks. `disabled` is the typed verdict a reader decides on (decision 0301), never the messages' words.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A strategy's failed hook calls over one run: [`StrategyFailures::record`] counts one,
/// [`StrategyFailures::merge`] folds another run's (a portfolio's mounts, a walk-forward's windows) in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrategyFailures {
    /// How many hook calls failed.
    pub count: u64,
    /// The strategy clock (`OrderBroker::now`, epoch ms) at the first failure; `None` when none failed,
    /// or when the first one failed with no clock to read (a restored state the strategy refused).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_at: Option<i64>,
    /// The first distinct messages, in the order first seen, at most [`StrategyFailures::MESSAGES_KEPT`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<String>,
    /// The strategy stopped running its hooks after its failures and placed no order from then on (a
    /// `.rhai` script after 10 errors in a row).
    #[serde(default)]
    pub disabled: bool,
}

impl StrategyFailures {
    /// How many distinct messages are kept: the first few say what broke, the rest repeat it.
    pub const MESSAGES_KEPT: usize = 5;

    /// One failed hook call, at strategy clock `at` (`None`: no clock to read), saying `message`.
    pub fn record(&mut self, at: Option<i64>, message: &str) {
        if self.count == 0 {
            self.first_at = at;
        }
        self.count += 1;
        self.keep(message);
    }

    /// `other`'s failures folded into these: the counts added, the earlier first time, the messages
    /// in order up to the bound, disabled when either is.
    pub fn merge(&mut self, other: &StrategyFailures) {
        self.count += other.count;
        self.first_at = match (self.first_at, other.first_at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        for message in &other.messages {
            self.keep(message);
        }
        self.disabled |= other.disabled;
    }

    /// Whether nothing failed: the report then carries no block (a clean report keeps its bytes).
    pub fn is_empty(&self) -> bool {
        self.count == 0 && !self.disabled
    }

    fn keep(&mut self, message: &str) {
        if self.messages.len() < Self::MESSAGES_KEPT && !self.messages.iter().any(|m| m == message)
        {
            self.messages.push(message.to_string());
        }
    }
}

/// The ONE loud line a report prints for a strategy that failed (`TradingResults`' table, `vike-cli`).
impl fmt::Display for StrategyFailures {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "STRATEGY FAILED {} time(s)", self.count)?;
        if let Some(at) = self.first_at {
            write!(f, ", first at {}", crate::time::epoch_ms_to_utc_timestamp(at))?;
        }
        if !self.messages.is_empty() {
            write!(f, ": {}", self.messages.join(" | "))?;
        }
        if self.disabled {
            f.write_str("; the strategy was DISABLED and placed no order after it")?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "failures_tests.rs"]
mod tests;
