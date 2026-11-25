//! **The ONE charge a history replay asks before it reads**, so the venue's REST read meter counts the connection
//! chain's reads (the reconnect replays of `crate::websocket_session::user::run_user_resync`, the gap-sentinel's of
//! `crate::exec_actor::run_loop`, each foreign symbol of `crate::execution_client::resync::replay_foreign`) and no bridge
//! knows there is a budget.
//!
//! The shape is `crate::link_history`'s: the gatewayhub installs ONE sink at boot (`crates/vike-gatewayhub/src/budget/reads.rs`,
//! over its one ledger), and the loops call [`charge`] before a read. No budget type crosses a layer: the sink takes the
//! venue, the symbol and the cost and answers admitted or not. A process with no sink (a test driver, a tool) admits
//! every read.
//!
//! **A refused read is SKIPPED, never queued**, and the next step of the chain covers it: a refused first reconnect replay
//! leaves the work to the delayed one, a refused delayed replay to the gap-sentinel and the reconcile pass, a refused
//! sentinel replay to its next window, a refused foreign symbol to the reconcile pass. Today's sink is the budget's
//! shadow mode (`crates/vike-gatewayhub/src/budget/mode.rs`'s `MODE`): it counts and admits everything.
//!
//! The cost is one per symbol's history read. shortcut: a symbol's reads (two or three requests) count one unit, because
//! no replay transport knows its venue's per-request weight (the binance family builds its replay clients with no
//! `RateGate`); upgrade when a venue states the weight of its history reads.

use std::sync::OnceLock;

/// What a sink is asked: `(venue, symbol, cost)`, answered `true` when the read may go to the venue.
pub type ReadBudgetSink = Box<dyn Fn(&str, &str, u64) -> bool + Send + Sync>;

static SINK: OnceLock<ReadBudgetSink> = OnceLock::new();

/// Give this process its budget. The first one stands: a daemon has one.
pub fn install(sink: ReadBudgetSink) {
    let _ = SINK.set(sink);
}

/// Charge `cost` read units of `venue`'s `symbol` before the read; `false` = refused, skip it (the module doc).
#[must_use]
pub fn charge(venue: &str, symbol: &str, cost: u64) -> bool {
    SINK.get().is_none_or(|sink| sink(venue, symbol, cost))
}

/// The symbol a test sink refuses: a read of any other symbol is admitted, so a sibling test in the same process reads
/// as it would with no sink.
#[cfg(test)]
pub(crate) const REFUSED_IN_TESTS: &str = "READ-BUDGET-REFUSED";

/// Install the test sink (once per process): it refuses [`REFUSED_IN_TESTS`] and admits every other read.
#[cfg(test)]
pub(crate) fn install_refusing_sink_for_tests() {
    install(Box::new(|_, symbol, _| symbol != REFUSED_IN_TESTS));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sink_answers_and_every_other_read_is_admitted() {
        install_refusing_sink_for_tests();
        assert!(!charge("bybit", REFUSED_IN_TESTS, 1));
        assert!(charge("bybit", "BTCUSDT", 1));
    }
}
